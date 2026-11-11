import { useCallback, useEffect, useRef, useState } from "react";
import {
  ApiError,
  api,
  LAYOUT_SCHEMA_VERSION,
  type GroupAccess,
  type LayoutDocument,
  type ModelPreset,
} from "./api";

/**
 * The editable form of a preset layout.
 *
 * The draft is the source of truth for rendering: every reorder, hide and restore lands
 * here first and is persisted by a 250 ms debounced `PUT /layout` carrying the revision the
 * client edited. A `409 preset_conflict` keeps the draft and pauses saving (the operator
 * loads the current revision or saves the draft as a new preset); any other failure rolls
 * the draft back to the last saved snapshot, so the table never keeps an order the gateway
 * rejected.
 *
 * The wire format is a `LayoutDocument`: items carry `provider`, `id`, a dense `position`
 * inside their visibility group, and `visible`. The page works in refs (`provider/id`),
 * which is what the registry rows key on, so the two conversions live here.
 */

/**
 * One row of the board. `last_visible_position` and `hidden_at` are the document's own
 * restore memory: they travel with the entry, so a reload keeps the slot a hidden row
 * came from.
 */
export type DraftEntry = {
  ref: string;
  visible: boolean;
  /** Explicit group id; absent means the provider bucket. */
  group?: string;
  last_visible_position?: number;
  hidden_at?: number;
};

/** A named group on the board. Provider groups keep the reserved `provider:` id. */
export type DraftGroup = {
  id: string;
  name: string;
  provider?: string;
  /** Whether the group and its models are advertised by `/v1/models`. */
  visible: boolean;
  /** Team/key/user grants; absent means the group is open to every caller. */
  access?: GroupAccess;
};

export type DraftState = "clean" | "dirty" | "saving" | "conflict";

export type LayoutDraft = {
  entries: DraftEntry[];
  groups: DraftGroup[];
  autoInclude: boolean;
  state: DraftState;
  savedAt: number | null;
  conflict: { message: string } | null;
  /** A save that was not a conflict: the draft was rolled back, and this says so. */
  error: string | null;
  /** Registry refs the preset has never seen; auto-include appends them to the draft. */
  adopt: (refs: string[]) => void;
  reorder: (from: string, to: string) => void;
  /** Create a free-form group and return its id. */
  createGroup: (name: string) => string;
  renameGroup: (id: string, name: string) => void;
  reorderGroup: (from: string, to: string) => void;
  /** Remove a free-form group; its models fall back to their provider buckets. */
  deleteGroup: (id: string) => void;
  /** Materialize provider buckets so they can be renamed and reordered. */
  ensureProviderGroups: (providers: Array<{ provider: string; name: string }>) => void;
  /** Move a model into a group; `null` returns it to its provider bucket. */
  assignGroup: (ref: string, group: string | null) => void;
  /** Toggle whether a group and its models are advertised by `/v1/models`. */
  setGroupVisible: (id: string, visible: boolean) => void;
  /** Show/hide one entry, adding it to the layout when it was never stored. */
  setVisible: (ref: string, visible: boolean) => void;
  hide: (ref: string) => void;
  restore: (ref: string) => void;
  /** Bring every hidden row back, and adopt registry refs the preset never stored. */
  restoreAll: (extraRefs?: string[]) => void;
  setAutoInclude: (next: boolean) => void;
  /** Flush the debounce now; resolves whether the gateway accepted the layout. */
  saveNow: () => Promise<boolean>;
  discard: () => void;
  /** Take the current server document after a conflict; the local draft is dropped. */
  loadLatest: () => void;
};

/** Visible rows first (in order), then the hidden ones: the two groups the store validates. */
export function entriesOf(layout: LayoutDocument | undefined): DraftEntry[] {
  const items = layout?.items ?? [];
  return [...items]
    .sort(
      (a, b) =>
        Number(b.visible) - Number(a.visible) || a.position - b.position,
    )
    .map((item) => ({
      ref: `${item.provider}/${item.id}`,
      visible: item.visible,
      ...(item.group === undefined ? {} : { group: item.group }),
      ...(item.last_visible_position === undefined
        ? {}
        : { last_visible_position: item.last_visible_position }),
      ...(item.hidden_at === undefined ? {} : { hidden_at: item.hidden_at }),
    }));
}

/** The explicit groups stored in a layout, in display order. */
export function groupsOf(layout: LayoutDocument | undefined): DraftGroup[] {
  return (layout?.groups ?? []).map((group) => ({
    ...group,
    visible: group.visible !== false,
  }));
}

/** The document the gateway validates: dense, zero-based positions per visibility group. */
export function layoutFor(
  entries: DraftEntry[],
  groups: DraftGroup[],
  autoInclude: boolean,
): LayoutDocument {
  const known = new Set(groups.map((group) => group.id));
  const items = (group: DraftEntry[]) =>
    group.map((entry, position) => {
      const cut = entry.ref.indexOf("/");
      return {
        provider: cut < 0 ? entry.ref : entry.ref.slice(0, cut),
        id: cut < 0 ? "" : entry.ref.slice(cut + 1),
        ...(entry.group && known.has(entry.group) ? { group: entry.group } : {}),
        position,
        visible: entry.visible,
        // Restore memory belongs to hidden rows only; a visible row carries none.
        ...(entry.visible || entry.last_visible_position === undefined
          ? {}
          : { last_visible_position: entry.last_visible_position }),
        ...(entry.visible || entry.hidden_at === undefined ? {} : { hidden_at: entry.hidden_at }),
      };
    });
  return {
    schema_version: LAYOUT_SCHEMA_VERSION,
    auto_include_new: autoInclude,
    groups: groups.map((group) => ({
      id: group.id,
      name: group.name,
      ...(group.provider === undefined ? {} : { provider: group.provider }),
      visible: group.visible,
      ...(group.access === undefined ? {} : { access: group.access }),
    })),
    items: [
      ...items(entries.filter((entry) => entry.visible)),
      ...items(entries.filter((entry) => !entry.visible)),
    ],
  };
}

/**
 * The groups to render: explicit groups first, then implicit provider buckets for
 * visible models that are not assigned to an explicit group. An implicit bucket is not in
 * the saved document until the operator customizes groups.
 */
export function effectiveGroups(
  entries: DraftEntry[],
  groups: DraftGroup[],
  nameOfProvider: (provider: string) => string,
): Array<DraftGroup & { implicit: boolean }> {
  const known = new Set(groups.map((group) => group.id));
  const byProvider = new Map(
    groups
      .filter((group) => group.provider)
      .map((group) => [group.provider as string, group.id]),
  );
  const out: Array<DraftGroup & { implicit: boolean }> = groups.map((group) => ({
    ...group,
    implicit: false,
  }));
  const seen = new Set(out.map((group) => group.id));
  for (const entry of entries) {
    if (!entry.visible) continue;
    if (entry.group && known.has(entry.group)) continue;
    const provider = entry.ref.slice(0, entry.ref.indexOf("/"));
    if (byProvider.has(provider)) continue;
    const id = `provider:${provider}`;
    if (seen.has(id)) continue;
    seen.add(id);
    out.push({
      id,
      name: nameOfProvider(provider),
      provider,
      visible: true,
      implicit: true,
    });
  }
  return out;
}

/** The group a visible row resolves to, including the implicit provider bucket. */
export function groupForEntry(
  entry: DraftEntry | undefined,
  groups: Array<DraftGroup & { implicit?: boolean }>,
): string {
  if (!entry) return "";
  if (entry.group && groups.some((group) => group.id === entry.group)) return entry.group;
  const provider = entry.ref.slice(0, entry.ref.indexOf("/"));
  return (
    groups.find((group) => group.provider === provider)?.id ?? `provider:${provider}`
  );
}

function insertAtVisible(
  entries: DraftEntry[],
  restored: DraftEntry,
  position: number,
): DraftEntry[] {
  const next = entries.filter((entry) => entry.ref !== restored.ref);
  const visible = next.filter((entry) => entry.visible);
  const anchor = visible[position];
  if (!anchor) {
    next.push({ ...restored, visible: true });
    return next;
  }
  const at = next.indexOf(anchor);
  next.splice(at, 0, { ...restored, visible: true });
  return next;
}

export function useLayoutDraft(
  preset: ModelPreset | undefined,
  /** Called after the gateway accepts a layout, so a list with counts can refresh. */
  onSaved?: () => void,
): LayoutDraft {
  const [entries, setEntries] = useState<DraftEntry[]>([]);
  const [groups, setGroups] = useState<DraftGroup[]>([]);
  const [autoInclude, setAutoIncludeState] = useState(false);
  const [state, setState] = useState<DraftState>("clean");
  const [savedAt, setSavedAt] = useState<number | null>(null);
  const [conflict, setConflict] = useState<LayoutDraft["conflict"]>(null);
  const [error, setError] = useState<string | null>(null);
  const [epoch, setEpoch] = useState(0);

  const saved = useRef<DraftEntry[]>([]);
  const savedGroups = useRef<DraftGroup[]>([]);
  const revision = useRef(0);
  const saves = useRef(0);
  const seeded = useRef<string | null>(null);
  const timer = useRef<number | null>(null);
  const dirty = state === "dirty" || state === "saving";

  // Seed once per (preset, revision). A refetch of the same revision never touches the
  // draft; a newer revision is ignored while the operator has unsaved edits, so their work
  // is not replaced — the next save gets the 409 this screen knows how to resolve.
  useEffect(() => {
    if (!preset) return;
    const key = `${preset.id}:${preset.revision}:${epoch}`;
    if (seeded.current === key) return;
    if (dirty) return;
    seeded.current = key;
    revision.current = preset.revision;
    saved.current = entriesOf(preset.layout);
    savedGroups.current = groupsOf(preset.layout);
    setEntries(saved.current);
    setGroups(savedGroups.current);
    setAutoIncludeState(preset.layout.auto_include_new);
    setConflict(null);
    setError(null);
    setSavedAt(null);
    setState("clean");
  }, [preset, epoch, dirty]);

  const persist = useCallback(
    async (next: DraftEntry[], nextGroups: DraftGroup[], auto: boolean): Promise<boolean> => {
      const id = preset?.id;
      if (!id) return false;
      const attempt = ++saves.current;
      setState("saving");
      return api
        .put<ModelPreset>(`/api/model-presets/${encodeURIComponent(id)}/layout`, {
          revision: revision.current,
          layout: layoutFor(next, nextGroups, auto),
        })
        .then((accepted) => {
          if (attempt !== saves.current) return false; // a newer save has already started
          revision.current = accepted.revision;
          saved.current = next;
          savedGroups.current = nextGroups;
          setConflict(null);
          setError(null);
          setState("clean");
          setSavedAt(Date.now());
          onSaved?.();
          return true;
        })
        .catch((failure: unknown) => {
          if (attempt !== saves.current) return false;
          if (failure instanceof ApiError && failure.status === 409) {
            setState("conflict");
            setConflict({
              message: "This layout was saved somewhere else. Your draft is kept.",
            });
            return false;
          }
          // Roll back to what the gateway last accepted, and say so: the table must not
          // keep an order the server rejected.
          setEntries(saved.current);
          setGroups(savedGroups.current);
          setState("clean");
          setError(failure instanceof Error ? failure.message : "the save failed");
          return false;
        });
    },
    [preset?.id, onSaved],
  );

  // One save 250 ms after the last change, not one per row moved.
  useEffect(() => {
    if (state !== "dirty" || conflict) return;
    if (timer.current !== null) window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => persist(entries, groups, autoInclude), 250);
    return () => {
      if (timer.current !== null) window.clearTimeout(timer.current);
      timer.current = null;
    };
  }, [entries, groups, autoInclude, state, conflict, persist]);

  const change = useCallback(
    (next: DraftEntry[], nextGroups: DraftGroup[] = groups) => {
      setEntries(next);
      setGroups(nextGroups);
      setError(null);
      setState((current) => (current === "conflict" ? current : "dirty"));
    },
    [groups],
  );

  return {
    entries,
    groups,
    autoInclude,
    state,
    savedAt,
    conflict,
    error,
    adopt: (refs) => {
      if (refs.length === 0) return;
      const known = new Set(entries.map((entry) => entry.ref));
      const added = refs.filter((ref) => !known.has(ref)).map((ref) => ({ ref, visible: true }));
      if (added.length === 0) return;
      change([...entries, ...added]);
    },
    reorder: (from, to) => {
      const start = entries.findIndex((entry) => entry.ref === from);
      const end = entries.findIndex((entry) => entry.ref === to);
      if (start < 0 || end < 0 || start === end) return;
      const next = [...entries];
      const [moved] = next.splice(start, 1);
      if (!moved) return;
      next.splice(end, 0, moved);
      change(next);
    },
    createGroup: (name) => {
      const id = `g-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
      change(entries, [
        ...groups,
        { id, name: name.trim() || "New group", visible: true },
      ]);
      return id;
    },
    renameGroup: (id, name) => {
      const clean = name.trim();
      if (!clean) return;
      change(
        entries,
        groups.map((group) => (group.id === id ? { ...group, name: clean } : group)),
      );
    },
    reorderGroup: (from, to) => {
      const start = groups.findIndex((group) => group.id === from);
      const end = groups.findIndex((group) => group.id === to);
      if (start < 0 || end < 0 || start === end) return;
      const next = [...groups];
      const [moved] = next.splice(start, 1);
      if (!moved) return;
      next.splice(end, 0, moved);
      change(entries, next);
    },
    deleteGroup: (id) => {
      if (groups.find((group) => group.id === id)?.provider) return;
      change(
        entries.map((entry) =>
          entry.group === id ? { ...entry, group: undefined } : entry,
        ),
        groups.filter((group) => group.id !== id),
      );
    },
    ensureProviderGroups: (providers) => {
      const known = new Set(
        groups.filter((group) => group.provider).map((group) => group.provider as string),
      );
      const added = providers
        .filter((provider) => !known.has(provider.provider))
        .map((provider) => ({
          id: `provider:${provider.provider}`,
          name: provider.name,
          provider: provider.provider,
          visible: true,
        }));
      if (added.length > 0) change(entries, [...groups, ...added]);
    },
    assignGroup: (ref, group) => {
      const known = entries.some((entry) => entry.ref === ref);
      const assigned = group === null ? { group: undefined } : { group };
      change(
        known
          ? entries.map((entry) =>
              entry.ref === ref ? { ...entry, ...assigned } : entry,
            )
          : [...entries, { ref, visible: true, ...assigned }],
      );
    },
    setGroupVisible: (id, visible) => {
      change(
        entries,
        groups.map((group) => (group.id === id ? { ...group, visible } : group)),
      );
    },
    setVisible: (ref, visible) => {
      const known = entries.some((entry) => entry.ref === ref);
      change(
        known
          ? entries.map((entry) => (entry.ref === ref ? { ...entry, visible } : entry))
          : [...entries, { ref, visible }],
      );
    },
    hide: (ref) => {
      const visible = entries.filter((entry) => entry.visible);
      const at = visible.findIndex((entry) => entry.ref === ref);
      if (at < 0) return;
      change(
        entries.map((entry) =>
          entry.ref === ref
            ? {
                ...entry,
                visible: false,
                last_visible_position: at,
                hidden_at: Math.floor(Date.now() / 1000),
              }
            : entry,
        ),
      );
    },
    restore: (ref) => {
      const entry = entries.find((candidate) => candidate.ref === ref);
      const position = entry?.last_visible_position;
      if (!entry) return;
      if (position === undefined) {
        change(entries.map((candidate) =>
          candidate.ref === ref ? { ...candidate, visible: true } : candidate,
        ));
        return;
      }
      // The remembered slot is the document's, so a reload restores to the same place.
      change(insertAtVisible(entries, entry, position));
    },
    restoreAll: (extraRefs = []) => {
      const known = new Set(entries.map((entry) => entry.ref));
      const added = extraRefs
        .filter((ref) => !known.has(ref))
        .map((ref) => ({ ref, visible: true }));
      change([
        ...entries.map((entry) => (entry.visible ? entry : { ...entry, visible: true })),
        ...added,
      ]);
    },
    setAutoInclude: (next) => {
      setAutoIncludeState(next);
      setError(null);
      setState((current) => (current === "conflict" ? current : "dirty"));
    },
    saveNow: () => {
      if (timer.current !== null) window.clearTimeout(timer.current);
      return persist(entries, groups, autoInclude);
    },
    discard: () => {
      if (timer.current !== null) window.clearTimeout(timer.current);
      setEntries(saved.current);
      setGroups(savedGroups.current);
      setAutoIncludeState(preset?.layout.auto_include_new ?? false);
      setConflict(null);
      setError(null);
      setState("clean");
    },
    loadLatest: () => {
      seeded.current = null;
      setEpoch((value) => value + 1);
    },
  };
}
