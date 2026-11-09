import { useMemo, useState } from "react";
import { Badge, Button, Card, ConfirmDialog, Field, Input, Modal, Select } from "./ui";
import { Checkbox } from "./atoms";
import { when } from "../lib/format";
import type { PresetSummary } from "../lib/api";
import type { DraftState } from "../lib/layout-draft";

/** What the manage dialog can ask the page to do; every call is a real endpoint. */
export type PresetManageActions = {
  create: (name: string) => void;
  rename: (id: string, name: string) => void;
  duplicate: (id: string, name: string) => void;
  remove: (id: string) => void;
};

const STATE_TEXT: Record<DraftState, string> = {
  clean: "saved",
  dirty: "unsaved changes",
  saving: "saving…",
  conflict: "not saved · conflict",
};

/**
 * The preset switcher.
 *
 * The select is the addressable route state (`?preset=`), the dot is the one signal that
 * the local draft differs from the stored layout, and every mutating action is disabled
 * while a save is in flight so the revision the server sees stays monotonic.
 */
export function PresetBar({
  presets,
  activeId,
  defaultId,
  draftState,
  savedAt,
  conflict,
  error,
  autoInclude,
  counts,
  updatedAt,
  busy,
  onSelect,
  onSave,
  onDiscard,
  onSaveAndSwitch,
  onSaveCopy,
  onAutoInclude,
  onMakeDefault,
  defaultBlocked,
  onLoadLatest,
  manage,
  unavailableNote,
}: {
  presets: PresetSummary[];
  activeId: string;
  defaultId: string;
  draftState: DraftState;
  savedAt: number | null;
  conflict: boolean;
  autoInclude: boolean;
  counts: string;
  updatedAt: number;
  busy: boolean;
  onSelect: (id: string) => void;
  onSave: () => void;
  onDiscard: () => void;
  onSaveAndSwitch: (id: string) => void;
  /** Set when a save rolled the draft back; the bar says so instead of pretending. */
  error?: string | null;
  /** Create a preset from the current draft (also the conflict banner's escape hatch). */
  onSaveCopy: (name: string) => void;
  onAutoInclude: (next: boolean) => void;
  /** Make this preset the instance default; the page owns the pending-endpoint probe. */
  onMakeDefault: (id: string) => void;
  /** Set once the gateway answers 404/405 for the default endpoint. */
  defaultBlocked: boolean;
  onLoadLatest: () => void;
  manage: PresetManageActions;
  unavailableNote?: string;
}) {
  const [naming, setNaming] = useState<
    | null
    | { kind: "create" }
    | { kind: "copy" }
    | { kind: "rename"; preset: PresetSummary }
    | { kind: "duplicate"; preset: PresetSummary }
  >(null);
  const [managing, setManaging] = useState(false);
  const [switching, setSwitching] = useState<string | null>(null);
  const [deleting, setDeleting] = useState<PresetSummary | null>(null);

  const current = useMemo(
    () => presets.find((preset) => preset.id === activeId) ?? null,
    [presets, activeId],
  );
  const dirty = draftState !== "clean";
  // The gateway refuses to delete the default (409 preset_is_default); show that up front.
  const canDelete = (preset: PresetSummary) => !preset.is_default;

  const pick = (id: string) => {
    if (id === activeId) return;
    if (dirty) setSwitching(id);
    else onSelect(id);
  };

  if (unavailableNote) {
    return (
      <Card className="mb-4">
        <div className="flex flex-wrap items-center gap-2 px-5 py-3 text-small text-muted">
          <Badge tone="neutral">presets</Badge>
          <span>{unavailableNote}</span>
        </div>
      </Card>
    );
  }

  return (
    <Card className="mb-4">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2 px-5 py-3">
        <label className="flex min-w-0 items-center gap-2">
          <span className="text-caption uppercase tracking-[0.08em] text-faint">preset</span>
          <span className="w-[210px] max-w-full">
            <Select ariaLabel="active preset" value={activeId} onChange={pick}>
              {presets.map((preset) => (
                <option key={preset.id} value={preset.id}>
                  {preset.name}
                  {preset.id === defaultId ? " · default" : ""}
                </option>
              ))}
            </Select>
          </span>
        </label>

        <span className="flex items-center gap-1.5 text-caption text-muted" role="status" aria-live="polite">
          {dirty ? (
            <span
              className="h-2 w-2 shrink-0 rounded-full bg-warn"
              title="the local draft differs from the stored layout"
              aria-hidden
            />
          ) : null}
          {STATE_TEXT[draftState]}
          {savedAt && draftState === "clean" ? ` · ${when(Math.floor(savedAt / 1000))}` : ""}
        </span>

        <span className="ml-auto flex flex-wrap items-center gap-1.5">
          <Button variant="primary" onClick={onSave} disabled={!dirty || draftState === "saving" || busy}>
            Save changes
          </Button>
          <Button variant="ghost" onClick={() => setNaming({ kind: "create" })} disabled={busy}>
            Save as new…
          </Button>
          <Button variant="quiet" onClick={() => setManaging(true)} disabled={busy}>
            Manage presets…
          </Button>
          <Button
            variant="quiet"
            onClick={() => current && onMakeDefault(current.id)}
            disabled={busy || !current || current.is_default || defaultBlocked}
            title={
              current?.is_default
                ? "This preset is already the default"
                : defaultBlocked
                  ? "This gateway build has no endpoint that changes the default preset"
                  : "New keys and callers without a preference fall back to the default"
            }
          >
            Make default
          </Button>
        </span>
      </div>

      <div className="flex flex-wrap items-center justify-between gap-2 border-t border-line px-5 py-2 text-caption text-faint">
        <span className="tnum">{counts}</span>
        <span className="flex flex-wrap items-center gap-3">
          {defaultBlocked && current && !current.is_default ? (
            <span className="text-[var(--color-warn)]">
              changing the default preset needs a newer gateway
            </span>
          ) : null}
          <span>{updatedAt ? `updated ${when(updatedAt)}` : "never saved"}</span>
          <label className="flex items-center gap-1.5">
            <Checkbox
              checked={autoInclude}
              onChange={onAutoInclude}
              ariaLabel="include new models automatically"
            />
            <span>include new models automatically</span>
          </label>
        </span>
      </div>

      {error ? (
        <div
          role="alert"
          className="border-t border-[var(--color-neg-line)] bg-[var(--color-neg-tint)] px-5 py-2 text-small text-[var(--color-neg-ink)]"
        >
          Could not save this layout ({error}); the previous order was restored. Try again, or
          keep editing.
        </div>
      ) : null}

      {conflict ? (
        <div role="alert" className="border-t border-[var(--color-warn-line)] bg-[var(--color-warn-tint)] px-5 py-3 text-small text-fg">
          <span className="font-medium">This layout was saved somewhere else.</span>{" "}
          <span className="text-muted">
            Your draft is kept. Load the stored layout, or save your draft as a new preset.
          </span>
          <span className="mt-2 flex flex-wrap gap-1.5">
            <Button variant="ghost" onClick={onLoadLatest}>
              Load latest
            </Button>
            <Button variant="primary" onClick={() => setNaming({ kind: "copy" })}>
              Save as copy…
            </Button>
          </span>
        </div>
      ) : null}

      <Modal
        open={switching !== null}
        variant="dialog"
        width="md"
        title="Save this layout first?"
        description="Switching presets would drop the unsaved order and visibility on this screen."
        onClose={() => setSwitching(null)}
        footer={
          <>
            <Button variant="quiet" onClick={() => setSwitching(null)}>
              Cancel
            </Button>
            <Button
              variant="ghost"
              onClick={() => {
                const target = switching;
                setSwitching(null);
                if (target) {
                  onDiscard();
                  onSelect(target);
                }
              }}
            >
              Discard and switch
            </Button>
            <Button
              variant="primary"
              onClick={() => {
                const target = switching;
                setSwitching(null);
                if (target) onSaveAndSwitch(target);
              }}
            >
              Save and switch
            </Button>
          </>
        }
      >
        <p className="text-body text-muted">
          {current ? current.name : "This preset"} has unsaved changes.
        </p>
      </Modal>

      <ManagePresetsDialog
        open={managing}
        presets={presets}
        activeId={activeId}
        defaultId={defaultId}
        canDelete={canDelete}
        onMakeDefaultRow={onMakeDefault}
        defaultBlockedRow={defaultBlocked}
        onClose={() => setManaging(false)}
        onDelete={(preset) => {
          setManaging(false);
          setDeleting(preset);
        }}
        onRename={(preset) => {
          setManaging(false);
          setNaming({ kind: "rename", preset });
        }}
        onDuplicate={(preset) => {
          setManaging(false);
          setNaming({ kind: "duplicate", preset });
        }}
        onCreate={() => {
          setManaging(false);
          setNaming({ kind: "create" });
        }}
      />

      <ConfirmDialog
        open={deleting !== null}
        title="Delete preset"
        message={
          <>
            Delete <span className="font-medium">{deleting?.name}</span>? Requests keep
            routing; only this saved order and visibility list is removed.
          </>
        }
        confirmLabel="Delete preset"
        onConfirm={() => {
          if (deleting) manage.remove(deleting.id);
          setDeleting(null);
        }}
        onClose={() => setDeleting(null)}
      />

      <NameDialog
        open={naming !== null}
        title={
          naming?.kind === "rename"
            ? "Rename preset"
            : naming?.kind === "duplicate"
              ? "Duplicate preset"
              : naming?.kind === "copy"
                ? "Save draft as a new preset"
                : "New preset"
        }
        description={
          naming?.kind === "copy"
            ? "Your unsaved layout becomes a new preset; the stored one is left alone."
            : naming?.kind === "rename"
              ? "The name is only a label; the entries stay."
              : naming?.kind === "duplicate"
                ? "A copy of this preset's entries, independent from now on."
                : "Starts from the current registry order."
        }
        initial={
          naming?.kind === "rename"
            ? naming.preset.name
            : naming?.kind === "duplicate"
              ? `${naming.preset.name} copy`
              : ""
        }
        submitLabel={
          naming?.kind === "rename"
            ? "Rename"
            : naming?.kind === "duplicate"
              ? "Duplicate"
              : "Create"
        }
        onSubmit={(name) => {
          if (!naming) return;
          if (naming.kind === "create") manage.create(name);
          else if (naming.kind === "copy") onSaveCopy(name);
          else if (naming.kind === "rename") manage.rename(naming.preset.id, name);
          else manage.duplicate(naming.preset.id, name);
          setNaming(null);
        }}
        onClose={() => setNaming(null)}
      />
    </Card>
  );
}

function NameDialog({
  open,
  title,
  description,
  initial,
  submitLabel,
  onSubmit,
  onClose,
}: {
  open: boolean;
  title: string;
  description: string;
  initial: string;
  submitLabel: string;
  onSubmit: (name: string) => void;
  onClose: () => void;
}) {
  const [name, setName] = useState(initial);
  const [seen, setSeen] = useState(initial);
  // Reopening for a different preset must not inherit the last typed name.
  if (open && initial !== seen) {
    setSeen(initial);
    setName(initial);
  }
  return (
    <Modal
      open={open}
      variant="dialog"
      width="md"
      title={title}
      description={description}
      onClose={onClose}
      footer={
        <>
          <Button variant="quiet" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="primary" onClick={() => onSubmit(name.trim())} disabled={name.trim() === ""}>
            {submitLabel}
          </Button>
        </>
      }
    >
      <Field label="name">
        <Input value={name} onChange={setName} ariaLabel="preset name" placeholder="production" />
      </Field>
    </Modal>
  );
}

function ManagePresetsDialog({
  open,
  presets,
  activeId,
  defaultId,
  canDelete,
  onMakeDefaultRow,
  defaultBlockedRow,
  onClose,
  onCreate,
  onRename,
  onDuplicate,
  onDelete,
}: {
  open: boolean;
  presets: PresetSummary[];
  activeId: string;
  defaultId: string;
  canDelete: (preset: PresetSummary) => boolean;
  onMakeDefaultRow: (id: string) => void;
  defaultBlockedRow: boolean;
  onClose: () => void;
  onCreate: () => void;
  onRename: (preset: PresetSummary) => void;
  onDuplicate: (preset: PresetSummary) => void;
  onDelete: (preset: PresetSummary) => void;
}) {
  return (
    <Modal
      open={open}
      variant="dialog"
      width="lg"
      title="Presets"
      description="A preset is an ordered list of models with your own visibility. Routing is unaffected by hiding a row."
      onClose={onClose}
      footer={
        <>
          <Button variant="quiet" onClick={onClose}>
            Close
          </Button>
          <Button variant="primary" onClick={onCreate}>
            New preset
          </Button>
        </>
      }
    >
      <ul aria-label="Presets" className="flex flex-col">
        {presets.map((preset) => (
          <li
            key={preset.id}
            className="flex flex-wrap items-center justify-between gap-2 border-b border-line py-2.5 last:border-b-0"
          >
            <div className="min-w-0">
              <div className="flex flex-wrap items-center gap-2">
                <span className="truncate text-small font-medium text-fg">{preset.name}</span>
                {preset.id === defaultId ? <Badge tone="accent">default</Badge> : null}
                {preset.id === activeId ? <Badge tone="pos">active</Badge> : null}
              </div>
              <div className="mt-0.5 text-caption text-faint tnum">
                {preset.entry_count} models · {preset.visible_count} visible · revision{" "}
                {preset.revision} · updated {when(preset.updated_at)}
                {preset.created_by ? ` · by ${preset.created_by}` : ""}
              </div>
            </div>
            <div className="flex flex-wrap items-center gap-1">
              <Button variant="quiet" onClick={() => onRename(preset)}>
                rename
              </Button>
              <Button variant="quiet" onClick={() => onDuplicate(preset)}>
                duplicate
              </Button>
              <Button
                variant="quiet"
                onClick={() => onMakeDefaultRow(preset.id)}
                disabled={preset.is_default || defaultBlockedRow}
                title={
                  preset.is_default
                    ? "This preset is already the default"
                    : defaultBlockedRow
                      ? "This gateway build has no endpoint that changes the default preset"
                      : undefined
                }
                ariaLabel={`Make ${preset.name} the default preset`}
              >
                make default
              </Button>
              <Button
                variant="danger"
                onClick={() => onDelete(preset)}
                disabled={!canDelete(preset)}
                title={preset.is_default ? "The default preset cannot be deleted" : undefined}
                ariaLabel={`Delete preset ${preset.name}`}
              >
                delete
              </Button>
            </div>
          </li>
        ))}
      </ul>
    </Modal>
  );
}
