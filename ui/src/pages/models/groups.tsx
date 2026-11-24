import { useEffect, useMemo, useState, type ReactNode } from "react";
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
import { PROVIDER_PILL } from "./rows";
import { groupDisplayName } from "./unified-rows";

/** Cap initial DOM rows in the group candidate checklist so 1,000+ models open instantly. */
const INITIAL_CANDIDATE_CAP = 100;

/** A section title above one group's table; the controls live in the group manager. */
export function GroupHeading({
  group,
  count,
  action,
}: {
  group: DraftGroup & { implicit?: boolean };
  count: number;
  action?: ReactNode;
}) {
  const title = groupDisplayName(group);
  return (
    <div className="flex flex-wrap items-center justify-between gap-2 border-b border-line bg-surface-2 px-3.5 py-2">
      <div className="flex min-w-0 items-center gap-1.5">
        <h3 className="truncate text-small font-semibold text-fg">{title}</h3>
        {!group.provider ? (
          <CopyIconButton value={group.name} label={`Copy group name ${group.name}`} />
        ) : null}
        {!group.provider ? (
          <span className="shrink-0 text-caption text-faint">· custom group</span>
        ) : null}
        <span
          className="shrink-0 text-caption text-faint tnum"
          aria-label={`${count.toLocaleString()} ${count === 1 ? "entry" : "entries"}`}
        >
          · {count.toLocaleString()}
        </span>
        {group.visible === false ? (
          <Badge tone="warn" title="This group and its models are not listed by /v1/models">
            hidden from /v1/models
          </Badge>
        ) : null}
      </div>
      {action ? <div className="shrink-0">{action}</div> : null}
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
  const [showAllCandidates, setShowAllCandidates] = useState(false);
  const [accessFor, setAccessFor] = useState<DraftGroup | null>(null);

  useEffect(() => {
    if (open) {
      setName("");
      setOpenModels(null);
      setFilter("");
      setShowAllCandidates(false);
    }
  }, [open]);

  // Index once so rendering large candidate lists stays O(N) instead of O(N²).
  const entryByRef = useMemo(
    () => new Map(entries.map((entry) => [entry.ref, entry] as const)),
    [entries],
  );
  const countByGroup = useMemo(() => {
    const counts = new Map<string, number>();
    for (const entry of entries) {
      if (entry.group) counts.set(entry.group, (counts.get(entry.group) ?? 0) + 1);
    }
    return counts;
  }, [entries]);

  const filteredCandidates = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    if (!needle) return candidates;
    return candidates.filter(
      (candidate) =>
        candidate.ref.toLowerCase().includes(needle) ||
        candidate.label.toLowerCase().includes(needle),
    );
  }, [candidates, filter]);

  const visibleCandidates = showAllCandidates
    ? filteredCandidates
    : filteredCandidates.slice(0, INITIAL_CANDIDATE_CAP);

  return (
    <Modal
      title="Model groups"
      description="Groups organize this preset's board and the default preset's /v1/models discovery. Adding a model to a group also shows it in this preset."
      open={open}
      onClose={onClose}
      width="lg"
      footer={<Button onClick={onClose}>Done</Button>}
    >
      {!isDefault ? (
        <p className="mb-3 rounded-[var(--radius-input)] border border-line bg-surface-2 px-3 py-2 text-caption text-muted">
          Editing a non-default preset. Group access grants and <span className="font-mono">/v1/models</span>{" "}
          discovery follow the instance default preset.
        </p>
      ) : null}
      <ul className="flex flex-col gap-2">
        {groups.map((group, index) => {
          const displayName = groupDisplayName(group);
          return (
          <li
            key={group.id}
            className="flex flex-wrap items-center gap-2 rounded-[var(--radius-card)] border border-line bg-surface-2 p-2"
          >
            <span className="min-w-0 flex-1">
              <GroupNameField name={displayName} onCommit={(next) => onRename(group.id, next)} />
            </span>
            <Badge tone={group.provider ? "neutral" : "accent"}>
              {group.provider ? "provider" : "custom"}
            </Badge>
            <Button
              variant="quiet"
              ariaLabel={`Access for ${displayName}`}
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
                label={`Show ${displayName} in /v1/models`}
                onChange={(next) => onSetVisible(group.id, next)}
              />
              /v1/models
            </span>
            {group.provider ? null : (
              <Button
                variant="quiet"
                aria-label={`${openModels === group.id ? "Close" : "Edit"} models in ${displayName}`}
                onClick={() => {
                  setOpenModels(openModels === group.id ? null : group.id);
                  setShowAllCandidates(false);
                }}
              >
                entries ({countByGroup.get(group.id) ?? 0})
              </Button>
            )}
            <Button
              variant="quiet"
              aria-label={`Move ${displayName} up`}
              disabled={index === 0}
              onClick={() => onReorder(group.id, groups[index - 1]?.id ?? group.id)}
            >
              ↑
            </Button>
            <Button
              variant="quiet"
              aria-label={`Move ${displayName} down`}
              disabled={index === groups.length - 1}
              onClick={() => onReorder(group.id, groups[index + 1]?.id ?? group.id)}
            >
              ↓
            </Button>
            {group.provider ? null : (
              <Button
                variant="quiet"
                aria-label={`Delete group ${displayName}`}
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
                  ariaLabel={`Filter entries for ${displayName}`}
                  onChange={(next) => {
                    setFilter(next);
                    setShowAllCandidates(false);
                  }}
                />
                <div className="mt-2 max-h-64 overflow-y-auto rounded-[var(--radius-input)] border border-line bg-surface">
                  {visibleCandidates.map((candidate) => {
                    const entry = entryByRef.get(candidate.ref);
                    const checked = entry?.group === group.id;
                    return (
                      <div
                        key={candidate.ref}
                        className="flex items-center gap-2 border-b border-[var(--color-rule)] px-2 py-1.5 last:border-b-0"
                      >
                        <Checkbox
                          checked={checked}
                          ariaLabel={
                            checked
                              ? `Remove ${candidate.ref} from ${displayName}`
                              : `Add ${candidate.ref} to ${displayName}, showing it in this preset`
                          }
                          onChange={(next) =>
                            onAssign(candidate.ref, next ? group.id : null)
                          }
                        />
                        <span className={PROVIDER_PILL}>{candidate.provider}</span>
                        <span className="min-w-0 flex-1 truncate font-mono text-caption">
                          {candidate.label}
                        </span>
                        {candidate.kind !== "model" ? (
                          <Badge tone="neutral">{candidate.kind}</Badge>
                        ) : null}
                        {entry && !entry.visible ? (
                          <Badge tone="warn">hidden</Badge>
                        ) : null}
                      </div>
                    );
                  })}
                  {filteredCandidates.length > visibleCandidates.length ? (
                    <div className="border-t border-line px-2 py-1.5 text-center">
                      <button
                        type="button"
                        onClick={() => setShowAllCandidates(true)}
                        className="text-caption text-accent-text underline underline-offset-2"
                      >
                        Show all {filteredCandidates.length.toLocaleString()} entries
                      </button>
                    </div>
                  ) : null}
                </div>
              </div>
            ) : null}
          </li>
          );
        })}
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
          Add group
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
      <div className="text-small font-medium text-muted">{title}</div>
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
      title={`Access · ${group.name}`}
      description="Who may use this group's /v1/models entry and its group/model alias."
      open
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" disabled={save.isPending} onClick={() => save.mutate()}>
            Save access
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
              "Teams",
              (teams.data ?? []).map((team) => ({ id: team.id, label: team.name })),
              teamIds,
              setTeamIds,
            )}
            {section(
              "API keys",
              (keys.data ?? []).map((key) => ({ id: key.id, label: key.name ?? key.id.slice(0, 8) })),
              keyIds,
              setKeyIds,
            )}
            {section(
              "Users",
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
