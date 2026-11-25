import { useMemo, useRef, useState } from "react";
import * as Popover from "@radix-ui/react-popover";
import { CaretDown, Check, Copy, DotsThree, PencilSimple, Plus, Star, Trash } from "@phosphor-icons/react";
import { Link } from "react-router-dom";
import { IconButton } from "../../components/atoms";
import { ModelChain, ModelSelect } from "../../components/model-select";
import { useToast } from "../../components/toast";
import {
  Badge,
  Button,
  ConfirmDialog,
  Dot,
  EmptyState,
  ErrorNote,
  Field,
  Input,
  Modal,
  Select,
  Textarea,
  cx,
} from "../../components/ui";
import { when } from "../../lib/format";
import {
  useCreatePreset,
  useDeletePreset,
  useDuplicatePreset,
  useMakeDefaultPreset,
  usePatchPreset,
} from "../../lib/queries";
import type { ComboStrategy, Model, ModelPreset, PresetSummary } from "../../lib/api";
import { normalisedPreview } from "../../lib/model-name";
import type { DraftState, LayoutDraft } from "../../lib/layout-draft";
import { presetOptionLabel } from "./params";

const STATE_TEXT: Record<DraftState, string> = {
  clean: "saved",
  dirty: "unsaved changes",
  saving: "saving…",
  conflict: "not saved · conflict",
};

const quietLink =
  "inline-flex h-8 shrink-0 items-center justify-center whitespace-nowrap rounded-full border border-line bg-surface px-3.5 text-body text-muted transition-colors hover:border-line-strong hover:bg-surface-3 hover:text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text";

/** What the preset form is editing; create/edit/duplicate share the same name validation. */
export type PresetFormState =
  | { kind: "create" }
  | { kind: "edit"; preset: PresetSummary }
  | { kind: "duplicate"; preset: PresetSummary };

/** Default preset first, then alphabetical by name. */
export function sortPresets(presets: PresetSummary[]): PresetSummary[] {
  return [...presets].sort(
    (a, b) => Number(b.is_default) - Number(a.is_default) || a.name.localeCompare(b.name),
  );
}

function submitFormLabel(kind: PresetFormState["kind"] | undefined, pending: boolean): string {
  if (pending) return "Working…";
  if (kind === "create") return "Create preset";
  if (kind === "edit") return "Save changes";
  return "Duplicate preset";
}

/**
 * Shared preset CRUD state & mutations powering both `PresetManagerDrawer` inside
 * `/models` and the standalone `/routing/presets` lifecycle page.
 */
export function usePresetCrud({
  onCreated,
  onDuplicated,
  onDeleted,
}: {
  onCreated?: (preset: ModelPreset) => void;
  onDuplicated?: (preset: ModelPreset) => void;
  onDeleted?: (id: string) => void;
} = {}) {
  const toast = useToast();
  const createPreset = useCreatePreset();
  const patchPreset = usePatchPreset();
  const duplicatePreset = useDuplicatePreset();
  const makeDefaultPreset = useMakeDefaultPreset();
  const deletePreset = useDeletePreset();

  const [form, setForm] = useState<PresetFormState | null>(null);
  const [formName, setFormName] = useState("");
  const [formDescription, setFormDescription] = useState("");
  const [deleting, setDeleting] = useState<PresetSummary | null>(null);
  const [defaultTarget, setDefaultTarget] = useState<PresetSummary | null>(null);

  const openForm = (next: PresetFormState) => {
    createPreset.reset();
    patchPreset.reset();
    duplicatePreset.reset();
    setForm(next);
    setFormName(next.kind === "create" ? "" : next.preset.name);
    setFormDescription(next.kind === "edit" ? (next.preset.description ?? "") : "");
  };

  const closeForm = () => {
    setForm(null);
    setFormName("");
    setFormDescription("");
  };

  const formNameValue = formName.trim();
  const formError = createPreset.error ?? patchPreset.error ?? duplicatePreset.error;
  const formPending =
    createPreset.isPending || patchPreset.isPending || duplicatePreset.isPending;

  const submitForm = () => {
    if (!form || formNameValue === "" || formPending) return;
    const description = formDescription.trim();
    if (form.kind === "create") {
      createPreset.mutate(
        { name: formNameValue, ...(description ? { description } : {}) },
        {
          onSuccess: (created) => {
            closeForm();
            toast.success("Preset created", created.name);
            onCreated?.(created);
          },
        },
      );
      return;
    }
    if (form.kind === "edit") {
      patchPreset.mutate(
        {
          id: form.preset.id,
          patch: { name: formNameValue, description: description === "" ? null : description },
        },
        {
          onSuccess: (updated) => {
            closeForm();
            toast.success("Preset updated", updated.name);
          },
        },
      );
      return;
    }
    duplicatePreset.mutate(
      { id: form.preset.id, name: formNameValue },
      {
        onSuccess: (created) => {
          closeForm();
          toast.success("Preset duplicated", created.name);
          onDuplicated?.(created);
        },
      },
    );
  };

  const formTitle =
    form?.kind === "create"
      ? "New preset"
      : form?.kind === "edit"
        ? "Rename preset"
        : "Duplicate preset";

  const formDescriptionText =
    form?.kind === "create"
      ? "A preset is a routing plan: the models it lists, their order, and their groups. The plan starts empty and merges the live catalog; Models is the editor."
      : form?.kind === "edit"
        ? "The name and description of a routing plan. Neither changes its layout or revision."
        : "A copy of the same routing plan under a new name. The copy starts with the same groups and order.";

  const openDelete = (preset: PresetSummary) => {
    deletePreset.reset();
    setDeleting(preset);
  };

  const closeDelete = () => {
    if (deletePreset.isPending) return;
    deletePreset.reset();
    setDeleting(null);
  };

  const confirmDelete = () => {
    const target = deleting;
    if (!target) return;
    deletePreset.mutate(target.id, {
      onSuccess: () => {
        setDeleting(null);
        toast.success("Preset deleted", target.name);
        onDeleted?.(target.id);
      },
    });
  };

  const openDefault = (preset: PresetSummary) => {
    makeDefaultPreset.reset();
    setDefaultTarget(preset);
  };

  const closeDefault = () => {
    if (makeDefaultPreset.isPending) return;
    makeDefaultPreset.reset();
    setDefaultTarget(null);
  };

  const confirmDefault = () => {
    const target = defaultTarget;
    if (!target) return;
    makeDefaultPreset.mutate(target.id, {
      onSuccess: () => {
        setDefaultTarget(null);
        toast.success("Default preset updated", target.name);
      },
    });
  };

  return {
    form,
    formName,
    setFormName,
    formDescription,
    setFormDescription,
    formNameValue,
    formError,
    formPending,
    formTitle,
    formDescriptionText,
    openForm,
    closeForm,
    submitForm,
    deleting,
    deletePending: deletePreset.isPending,
    deleteError: deletePreset.error,
    openDelete,
    closeDelete,
    confirmDelete,
    defaultTarget,
    defaultPending: makeDefaultPreset.isPending,
    defaultError: makeDefaultPreset.error,
    openDefault,
    closeDefault,
    confirmDefault,
  };
}

/** Shared name & description inputs used by both the inline drawer editor and the dialog modal. */
export function PresetFormFields({
  form,
  name,
  onNameChange,
  description,
  onDescriptionChange,
  pending,
  error,
  onSubmit,
}: {
  form: PresetFormState;
  name: string;
  onNameChange: (next: string) => void;
  description: string;
  onDescriptionChange: (next: string) => void;
  pending: boolean;
  error: unknown;
  onSubmit: () => void;
}) {
  return (
    <div className="flex flex-col gap-4">
      {error ? <ErrorNote error={error} /> : null}
      <Field
        label="Name"
        required
        hint={
          form.kind === "duplicate"
            ? `A new name for the copy of ${form.preset.name}.`
            : "Shown in the preset switcher on Models."
        }
      >
        <Input
          value={name}
          onChange={onNameChange}
          autoFocus
          placeholder="e.g. Production"
          ariaLabel="Preset name"
          onKeyDown={(event) => {
            if (event.key === "Enter" && name.trim() !== "" && !pending) {
              event.preventDefault();
              onSubmit();
            }
          }}
        />
      </Field>
      {form.kind === "create" || form.kind === "edit" ? (
        <Field
          label="Description"
          optional
          hint="What this plan is for and who should use it."
        >
          <Textarea
            value={description}
            onChange={onDescriptionChange}
            placeholder="e.g. Fallback chain for production traffic"
            ariaLabel="Preset description"
            rows={3}
          />
        </Field>
      ) : null}
    </div>
  );
}

/** Standalone modal wrapper around `PresetFormFields` (used on `/routing/presets`). */
export function PresetFormModal({
  form,
  title,
  descriptionText,
  name,
  onNameChange,
  description,
  onDescriptionChange,
  nameValue,
  pending,
  error,
  onClose,
  onSubmit,
}: {
  form: PresetFormState | null;
  title: string;
  descriptionText: string;
  name: string;
  onNameChange: (next: string) => void;
  description: string;
  onDescriptionChange: (next: string) => void;
  nameValue: string;
  pending: boolean;
  error: unknown;
  onClose: () => void;
  onSubmit: () => void;
}) {
  return (
    <Modal
      open={form !== null}
      onClose={onClose}
      title={title}
      description={descriptionText}
      variant="dialog"
      footer={
        <>
          <Button onClick={onClose} disabled={pending}>
            Cancel
          </Button>
          <Button
            variant="primary"
            arrow={false}
            loading={pending}
            disabled={pending || nameValue === ""}
            onClick={onSubmit}
          >
            {submitFormLabel(form?.kind, pending)}
          </Button>
        </>
      }
    >
      {form ? (
        <PresetFormFields
          key={form.kind === "create" ? "create" : `${form.kind}:${form.preset.id}`}
          form={form}
          name={name}
          onNameChange={onNameChange}
          description={description}
          onDescriptionChange={onDescriptionChange}
          pending={pending}
          error={error}
          onSubmit={onSubmit}
        />
      ) : null}
    </Modal>
  );
}

/** Shared confirmation dialog for promoting a preset to the instance default. */
export function PresetDefaultDialog({
  target,
  busy,
  error,
  onClose,
  onConfirm,
}: {
  target: PresetSummary | null;
  busy: boolean;
  error: unknown;
  onClose: () => void;
  onConfirm: () => void;
}) {
  return (
    <Modal
      open={target !== null}
      onClose={onClose}
      title="Make this preset the default?"
      description="New keys and callers without a preference fall back to the default plan."
      variant="dialog"
      footer={
        <>
          <Button onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="primary" arrow={false} loading={busy} disabled={busy} onClick={onConfirm}>
            {busy ? "Working…" : "Make default"}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-3">
        {error ? <ErrorNote error={error} /> : null}
        <p className="text-body leading-relaxed text-ink-soft">
          <span className="font-semibold text-fg">{target?.name}</span> becomes the instance
          default. The current default keeps its layout and stays available; only the fallback
          changes.
        </p>
      </div>
    </Modal>
  );
}

/** Shared Tier-3 destructive confirmation dialog for deleting a preset. */
export function PresetDeleteDialog({
  target,
  busy,
  error,
  onClose,
  onConfirm,
}: {
  target: PresetSummary | null;
  busy: boolean;
  error: unknown;
  onClose: () => void;
  onConfirm: () => void;
}) {
  return (
    <ConfirmDialog
      open={target !== null}
      title="Delete this preset?"
      confirmLabel="Delete preset"
      busy={busy}
      error={error}
      message={
        <>
          <span className="font-semibold text-fg">{target?.name}</span> and its saved order,
          groups and visibility are removed. Models keep routing through the default preset.
          {target?.is_default ? (
            <span className="mt-2 block">
              This is the default preset, and the gateway refuses to delete it. Make another preset
              the default first.
            </span>
          ) : null}
        </>
      }
      onClose={onClose}
      onConfirm={onConfirm}
    />
  );
}

/** Shared dirty-draft confirmation modal before switching the active preset in `/models`. */
function SwitchLayoutConfirmModal({
  open,
  onCancel,
  onDiscard,
  onSaveAndSwitch,
}: {
  open: boolean;
  onCancel: () => void;
  onDiscard: () => void;
  onSaveAndSwitch: () => void;
}) {
  return (
    <Modal
      open={open}
      variant="dialog"
      width="md"
      title="Save this layout first?"
      description="Switching presets would drop the unsaved order and visibility on this screen."
      onClose={onCancel}
      footer={
        <>
          <Button variant="quiet" onClick={onCancel}>
            Cancel
          </Button>
          <Button variant="ghost" onClick={onDiscard}>
            Discard changes
          </Button>
          <Button variant="primary" onClick={onSaveAndSwitch}>
            Save & switch
          </Button>
        </>
      }
    >
      <p className="text-body text-muted">
        The selected preset becomes the one this account edits. Saving writes the current draft to
        the preset you are leaving.
      </p>
    </Modal>
  );
}

/**
 * The preset line directly under the `/models` page title. Switching and saving happen inline;
 * `Save` appears only when the draft has unsaved changes. Full preset lifecycle management opens
 * `PresetManagerDrawer` without navigating away.
 */
export function PresetControls({
  presets,
  presetId,
  defaultId,
  draft,
  busy,
  onSelect,
  onSaveAsNew,
  onManagePresets,
  onManageGroups,
}: {
  presets: PresetSummary[];
  presetId: string;
  defaultId: string;
  draft: LayoutDraft;
  /** An activation or a pending library mutation. */
  busy: boolean;
  onSelect: (id: string) => void;
  onSaveAsNew: () => void;
  onManagePresets?: () => void;
  onManageGroups?: () => void;
}) {
  const [switching, setSwitching] = useState<string | null>(null);
  const [pickerOpen, setPickerOpen] = useState(false);
  const dirty = draft.state !== "clean";
  const saving = draft.state === "saving";
  const conflict = draft.conflict !== null;
  const showSave = (draft.state === "dirty" || draft.state === "saving") && !conflict;
  const currentPreset = presets.find((preset) => preset.id === presetId);
  const currentLabel = currentPreset
    ? presetOptionLabel(currentPreset.name, currentPreset.id === defaultId)
    : "Preset";

  const pick = (id: string) => {
    setPickerOpen(false);
    if (id === presetId) return;
    if (dirty) setSwitching(id);
    else onSelect(id);
  };

  const saveAndSwitch = async () => {
    const id = switching;
    if (id === null) return;
    setSwitching(null);
    if (await draft.saveNow()) onSelect(id);
  };

  const discardAndSwitch = () => {
    const id = switching;
    if (id === null) return;
    draft.discard();
    setSwitching(null);
    onSelect(id);
  };

  return (
    <div className="-mt-3 mb-4 flex flex-wrap items-center justify-between gap-x-4 gap-y-2">
      <div className="flex min-w-0 flex-wrap items-center gap-2">
        <span className="text-small text-muted">Preset</span>

        <Popover.Root open={pickerOpen} onOpenChange={setPickerOpen}>
          <Popover.Trigger asChild>
            <button
              type="button"
              disabled={busy}
              aria-label="Preset being edited"
              aria-haspopup="dialog"
              aria-expanded={pickerOpen}
              className="inline-flex h-8 max-w-[220px] items-center gap-1.5 rounded-full border border-line bg-surface px-3 text-small font-medium text-fg transition-colors hover:border-[var(--color-line-strong)] hover:bg-surface-3 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)] disabled:cursor-not-allowed disabled:opacity-60"
            >
              <span className="truncate">{currentLabel}</span>
              <CaretDown size={11} className="shrink-0 text-muted" aria-hidden />
            </button>
          </Popover.Trigger>
          <Popover.Portal>
            <Popover.Content
              role="group"
              aria-label="Choose preset"
              side="bottom"
              align="start"
              sideOffset={4}
              className="popover-panel z-[var(--z-popover-portal)] flex w-60 flex-col gap-0.5 rounded-[var(--radius-card)] border border-line bg-surface p-1 shadow-[var(--shadow-popover)]"
            >
              {presets.map((preset) => {
                const selected = preset.id === presetId;
                const label = presetOptionLabel(preset.name, preset.id === defaultId);
                return (
                  <button
                    key={preset.id}
                    type="button"
                    aria-pressed={selected}
                    onClick={() => pick(preset.id)}
                    className={cx(
                      "flex h-8 w-full items-center justify-between gap-2 rounded-[var(--radius-input)] px-2.5 text-left text-small transition-colors hover:bg-surface-3 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-[-2px] focus-visible:outline-[var(--color-focus-ring)]",
                      selected ? "bg-[var(--color-selected)] font-medium text-fg" : "text-fg",
                    )}
                  >
                    <span className="truncate">{label}</span>
                    {selected ? (
                      <Check size={14} weight="bold" className="shrink-0 text-accent-text" aria-hidden />
                    ) : null}
                  </button>
                );
              })}
            </Popover.Content>
          </Popover.Portal>
        </Popover.Root>

        <span
          role="status"
          aria-live="polite"
          className="flex items-center gap-1.5 text-caption text-muted"
        >
          {conflict ? (
            <Dot tone="neg" />
          ) : dirty ? (
            <Dot tone="warn" />
          ) : (
            <Check size={13} weight="bold" className="text-[var(--color-pos)]" aria-hidden />
          )}
          {STATE_TEXT[draft.state]}
          {!dirty && draft.savedAt ? ` · ${when(Math.floor(draft.savedAt / 1000))}` : ""}
        </span>

        {showSave ? (
          <Button
            variant="primary"
            size="md"
            loading={saving}
            onClick={() => void draft.saveNow()}
            disabled={saving || busy}
            title="Write this order and visibility to the preset"
          >
            Save
          </Button>
        ) : null}
      </div>

      <div className="flex shrink-0 items-center gap-1">
        {onManagePresets ? (
          <Button variant="ghost" size="sm" onClick={onManagePresets} className="text-muted hover:text-fg">
            Manage presets
          </Button>
        ) : (
          <Link
            to="/routing/presets"
            className="inline-flex h-7 shrink-0 items-center justify-center whitespace-nowrap rounded-full px-2.5 text-small text-muted transition-colors hover:bg-surface-3 hover:text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
          >
            Manage presets
          </Link>
        )}

        <Popover.Root>
          <Popover.Trigger asChild>
            <button
              type="button"
              aria-label="Preset actions"
              aria-haspopup="menu"
              className="flex h-7 w-7 shrink-0 items-center justify-center rounded-full text-muted transition-colors hover:bg-surface-3 hover:text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--color-focus-ring)]"
            >
              <DotsThree size={18} weight="bold" aria-hidden />
            </button>
          </Popover.Trigger>
          <Popover.Portal>
            <Popover.Content
              role="menu"
              aria-label="Preset actions"
              side="bottom"
              align="end"
              sideOffset={4}
              className="popover-panel z-[var(--z-popover-portal)] flex w-56 flex-col rounded-[var(--radius-card)] border border-line bg-surface p-1 shadow-[var(--shadow-popover)]"
            >
              <Popover.Close asChild>
                <button type="button" role="menuitem" onClick={onSaveAsNew} className={menuItem}>
                  Save as new…
                </button>
              </Popover.Close>
              {onManageGroups ? (
                <Popover.Close asChild>
                  <button type="button" role="menuitem" onClick={onManageGroups} className={menuItem}>
                    Manage groups…
                  </button>
                </Popover.Close>
              ) : null}
              <Popover.Close asChild>
                {onManagePresets ? (
                  <button type="button" role="menuitem" onClick={onManagePresets} className={menuItem}>
                    Manage presets…
                  </button>
                ) : (
                  <Link to="/routing/presets" role="menuitem" className={menuItem}>
                    Manage presets…
                  </Link>
                )}
              </Popover.Close>
              <Popover.Close asChild>
                <button
                  type="button"
                  role="menuitem"
                  onClick={draft.discard}
                  disabled={!dirty}
                  className={menuItem}
                >
                  Discard changes
                </button>
              </Popover.Close>
            </Popover.Content>
          </Popover.Portal>
        </Popover.Root>
      </div>

      <SwitchLayoutConfirmModal
        open={switching !== null}
        onCancel={() => setSwitching(null)}
        onDiscard={discardAndSwitch}
        onSaveAndSwitch={() => void saveAndSwitch()}
      />
    </div>
  );
}

/**
 * In-page slide-over drawer for creating, renaming, duplicating, deleting, and
 * setting the default preset directly inside `/models`.
 */
export function PresetManagerDrawer({
  open,
  onClose,
  presets,
  activePresetId,
  defaultId,
  draft,
  busy = false,
  onSelectPreset,
  onDeletedPreset,
}: {
  open: boolean;
  onClose: () => void;
  presets: PresetSummary[];
  activePresetId: string | null;
  defaultId: string;
  draft?: LayoutDraft;
  busy?: boolean;
  onSelectPreset: (id: string) => void;
  onDeletedPreset?: (id: string) => void;
}) {
  const [switching, setSwitching] = useState<string | null>(null);
  const crud = usePresetCrud({
    onDeleted: (deletedId) => onDeletedPreset?.(deletedId),
  });

  const rows = useMemo(() => sortPresets(presets), [presets]);
  const dirty = draft !== undefined && draft.state !== "clean";

  const handleClose = () => {
    crud.closeForm();
    setSwitching(null);
    onClose();
  };

  const requestSwitch = (id: string) => {
    if (id === activePresetId) return;
    if (dirty) {
      setSwitching(id);
      return;
    }
    onSelectPreset(id);
  };

  const saveAndSwitch = async () => {
    const id = switching;
    if (id === null || !draft) return;
    setSwitching(null);
    if (await draft.saveNow()) onSelectPreset(id);
  };

  const discardAndSwitch = () => {
    const id = switching;
    if (id === null || !draft) return;
    draft.discard();
    setSwitching(null);
    onSelectPreset(id);
  };

  const submitLabel = submitFormLabel(crud.form?.kind, crud.formPending);

  return (
    <>
      <Modal
        open={open}
        onClose={handleClose}
        variant="drawer"
        width="lg"
        title="Manage presets"
        description="Create, rename, duplicate, delete, or set the default routing plan without leaving Models."
        onEscapeKeyDown={(event) => {
          if (crud.form !== null) {
            event.preventDefault();
            crud.closeForm();
          }
        }}
        footer={
          <>
            <Link to="/routing/presets" onClick={handleClose} className={quietLink}>
              Open Presets page →
            </Link>
            <Button variant="secondary" onClick={handleClose}>
              Done
            </Button>
          </>
        }
      >
        <div className="flex flex-col gap-4">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div className="flex flex-wrap items-center gap-2">
              <span className="text-small text-muted tnum">
                {rows.length} preset{rows.length === 1 ? "" : "s"}
              </span>
              {dirty ? <Badge tone="warn">unsaved layout draft</Badge> : null}
            </div>
            <Button
              variant="primary"
              size="sm"
              disabled={crud.formPending}
              onClick={() => crud.openForm({ kind: "create" })}
            >
              <Plus size={14} aria-hidden />
              New preset
            </Button>
          </div>

          {crud.form ? (
            <section
              key={
                crud.form.kind === "create"
                  ? "create"
                  : `${crud.form.kind}:${crud.form.preset.id}`
              }
              aria-label={crud.formTitle}
              className="rounded-[var(--radius-surface)] border border-line-strong bg-surface-2 p-4"
            >
              <div className="mb-3">
                <h3 className="text-body font-semibold text-fg">{crud.formTitle}</h3>
                <p className="mt-0.5 text-caption text-muted">{crud.formDescriptionText}</p>
              </div>
              <PresetFormFields
                form={crud.form}
                name={crud.formName}
                onNameChange={crud.setFormName}
                description={crud.formDescription}
                onDescriptionChange={crud.setFormDescription}
                pending={crud.formPending}
                error={crud.formError}
                onSubmit={crud.submitForm}
              />
              <div className="mt-4 flex flex-wrap items-center justify-end gap-2">
                <Button
                  variant="quiet"
                  size="sm"
                  onClick={crud.closeForm}
                  disabled={crud.formPending}
                >
                  Cancel
                </Button>
                <Button
                  variant="primary"
                  size="sm"
                  loading={crud.formPending}
                  disabled={crud.formPending || crud.formNameValue === ""}
                  onClick={crud.submitForm}
                >
                  {submitLabel}
                </Button>
              </div>
            </section>
          ) : null}

          {rows.length === 0 ? (
            <div className="rounded-[var(--radius-card)] border border-line bg-surface">
              <EmptyState>
                <div className="flex flex-col items-center gap-2">
                  <span>No presets yet. Create one to start a routing plan.</span>
                  <Button
                    variant="primary"
                    size="sm"
                    onClick={() => crud.openForm({ kind: "create" })}
                  >
                    <Plus size={14} aria-hidden />
                    New preset
                  </Button>
                </div>
              </EmptyState>
            </div>
          ) : (
            <ul className="overflow-hidden rounded-[var(--radius-card)] border border-line bg-surface">
              {rows.map((preset) => {
                const isActive = preset.id === activePresetId;
                const isDefault = preset.is_default || preset.id === defaultId;
                return (
                  <li
                    key={preset.id}
                    className={cx(
                      "flex flex-wrap items-center justify-between gap-3 border-t border-line px-4 py-3 first:border-t-0",
                      isActive && "bg-[var(--color-selected)]/40",
                    )}
                  >
                    <div className="min-w-0 flex-1">
                      <div className="flex flex-wrap items-center gap-1.5">
                        <span className="truncate text-body font-medium text-fg">
                          {preset.name}
                        </span>
                        {isDefault ? <Badge tone="pos">default</Badge> : null}
                        {isActive ? <Badge tone="accent">active</Badge> : null}
                      </div>
                      {preset.description ? (
                        <p
                          className="mt-0.5 truncate text-caption text-muted"
                          title={preset.description}
                        >
                          {preset.description}
                        </p>
                      ) : null}
                      <div className="mt-1 flex flex-wrap items-center gap-2 text-caption text-faint tnum">
                        <span>
                          {preset.visible_count} / {preset.entry_count} models
                        </span>
                        <span>· rev {preset.revision}</span>
                        <span>· {when(preset.updated_at)}</span>
                      </div>
                    </div>
                    <div className="flex shrink-0 items-center gap-1">
                      <Button
                        variant={isActive ? "quiet" : "secondary"}
                        size="sm"
                        disabled={isActive || busy}
                        onClick={() => requestSwitch(preset.id)}
                      >
                        {isActive ? "Editing" : "Switch"}
                      </Button>
                      <IconButton
                        label={`Rename ${preset.name}`}
                        onClick={() => crud.openForm({ kind: "edit", preset })}
                      >
                        <PencilSimple size={15} aria-hidden />
                      </IconButton>
                      <IconButton
                        label={`Duplicate ${preset.name}`}
                        onClick={() => crud.openForm({ kind: "duplicate", preset })}
                      >
                        <Copy size={15} aria-hidden />
                      </IconButton>
                      <IconButton
                        label={`Make ${preset.name} the default`}
                        disabled={isDefault}
                        title={
                          isDefault
                            ? "This preset is already the default"
                            : "New keys and callers without a preference fall back to the default"
                        }
                        onClick={() => crud.openDefault(preset)}
                      >
                        <Star size={15} weight={isDefault ? "fill" : "regular"} aria-hidden />
                      </IconButton>
                      <IconButton
                        label={`Delete ${preset.name}`}
                        onClick={() => crud.openDelete(preset)}
                        className="hover:bg-[var(--color-neg-tint)] hover:text-[var(--color-neg-ink)]"
                      >
                        <Trash size={15} aria-hidden />
                      </IconButton>
                    </div>
                  </li>
                );
              })}
            </ul>
          )}
        </div>
      </Modal>

      <SwitchLayoutConfirmModal
        open={switching !== null}
        onCancel={() => setSwitching(null)}
        onDiscard={discardAndSwitch}
        onSaveAndSwitch={() => void saveAndSwitch()}
      />

      <PresetDefaultDialog
        target={crud.defaultTarget}
        busy={crud.defaultPending}
        error={crud.defaultError}
        onClose={crud.closeDefault}
        onConfirm={crud.confirmDefault}
      />

      <PresetDeleteDialog
        target={crud.deleting}
        busy={crud.deletePending}
        error={crud.deleteError}
        onClose={crud.closeDelete}
        onConfirm={crud.confirmDelete}
      />
    </>
  );
}

/** The one primary action on the page: add a row the registry cannot sync. */
export function AddMenu({
  onAddCustom,
  onAddAlias,
  onAddCombo,
}: {
  onAddCustom: () => void;
  /** Opens the alias editor on this page, so a new alias can be grouped as it is made. */
  onAddAlias: () => void;
  /** Opens the combo editor on this page, so a new combo can be grouped as it is made. */
  onAddCombo: () => void;
}) {
  return (
    <Popover.Root>
      <Popover.Trigger asChild>
        <button
          type="button"
          aria-label="Add a model, alias or combo"
          aria-haspopup="menu"
          className="inline-flex h-8 shrink-0 items-center justify-center gap-1.5 whitespace-nowrap rounded-full border border-transparent bg-accent px-3.5 text-body font-semibold text-[var(--color-accent-ink)] transition-colors hover:bg-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text"
        >
          <Plus size={14} weight="bold" aria-hidden />
          Add
          <CaretDown size={11} aria-hidden />
        </button>
      </Popover.Trigger>
      <Popover.Portal>
        <Popover.Content
          role="menu"
          aria-label="Add a row"
          side="bottom"
          align="end"
          sideOffset={4}
          className="popover-panel z-[var(--z-popover-portal)] flex w-56 flex-col rounded-[var(--radius-card)] border border-line bg-surface p-1 shadow-[var(--shadow-popover)]"
        >
          <Popover.Close asChild>
            <button type="button" role="menuitem" onClick={onAddCustom} className={menuItem}>
              Custom model
            </button>
          </Popover.Close>
          <Popover.Close asChild>
            <button type="button" role="menuitem" onClick={onAddAlias} className={menuItem}>
              Alias
            </button>
          </Popover.Close>
          <Popover.Close asChild>
            <button type="button" role="menuitem" onClick={onAddCombo} className={menuItem}>
              Combo
            </button>
          </Popover.Close>
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
  );
}

/** What the page needs to create an alias and place it in the active preset draft. */
export type AliasCreatePayload = {
  alias: string;
  target: string;
  group: string | null;
};

/**
 * Add → Alias, without leaving the preset. Same draft-assignment flow as `ComboCreateDialog`
 * so a new alias lands in the chosen group without a second step.
 */
export function AliasCreateDialog({
  groups,
  editable,
  groupDisabledReason,
  busy,
  error,
  onClose,
  onSubmit,
}: {
  groups: Array<{ id: string; name: string }>;
  editable: boolean;
  groupDisabledReason: string | null;
  busy: boolean;
  error: unknown;
  onClose: () => void;
  onSubmit: (payload: AliasCreatePayload) => void;
}) {
  const form = useRef<HTMLFormElement>(null);
  const [alias, setAlias] = useState("");
  const [target, setTarget] = useState("");
  const [group, setGroup] = useState("");
  const ready = alias.trim() !== "" && target !== "" && !busy;

  const submit = () => {
    if (!ready) return;
    onSubmit({
      alias: alias.trim(),
      target,
      group: group === "" ? null : group,
    });
  };

  return (
    <Modal
      open
      title="New alias"
      description="A stable model name clients send. It points at one model and joins the group you choose."
      width="md"
      onClose={onClose}
      footer={
        <>
          <Button variant="quiet" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button
            variant="primary"
            onClick={() => form.current?.requestSubmit()}
            disabled={!ready}
          >
            {busy ? "Creating…" : "Create alias"}
          </Button>
        </>
      }
    >
      <form
        ref={form}
        className="flex flex-col gap-4"
        onSubmit={(event) => {
          event.preventDefault();
          submit();
        }}
      >
        <Field label="alias name" hint="the name clients send">
          <Input value={alias} onChange={setAlias} placeholder="fast" ariaLabel="alias name" autoFocus />
          <NamePreviewHint name={alias} />
        </Field>
        <Field label="target model" hint="the model this alias resolves to">
          <ModelSelect
            ariaLabel="target model"
            value={target}
            onChange={setTarget}
            allowEmpty="choose a model"
          />
        </Field>
        <Field
          label="preset group"
          hint={
            editable
              ? "The alias is added to this group in the active preset."
              : (groupDisabledReason ?? "Open a preset to assign a group.")
          }
        >
          <Select ariaLabel="preset group" value={group} onChange={setGroup} disabled={!editable}>
            <option value="">Ungrouped (RRouter)</option>
            {groups.map((entry) => (
              <option key={entry.id} value={entry.id}>
                {entry.name}
              </option>
            ))}
          </Select>
        </Field>
        {error ? <ErrorNote error={error} /> : null}
      </form>
    </Modal>
  );
}

const menuItem =
  "flex h-8 items-center rounded-[var(--radius-input)] px-2 text-left text-small text-fg hover:bg-surface-3 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-[-2px] focus-visible:outline-accent-text disabled:cursor-not-allowed disabled:text-faint";

/** What the page needs to create the combo and place it in the active preset draft. */
export type ComboCreatePayload = {
  name: string;
  models: string[];
  strategy: ComboStrategy;
  router_model?: string;
  /** A custom group id, or `null` for the RRouter bucket. */
  group: string | null;
};

const STRATEGY_CHOICES: Array<{ id: ComboStrategy; label: string }> = [
  { id: "fallback", label: "Fallback · try the candidates in order" },
  { id: "round_robin", label: "Round robin · rotate the starting candidate" },
  { id: "smart", label: "Smart · a router model picks the first candidate" },
];

/** The API's own rules, checked here so a bad chain is a field message, not a 400. */
function comboProblem(
  name: string,
  candidates: string[],
  strategy: ComboStrategy,
  routerModel: string,
): string | null {
  const trimmed = name.trim();
  if (trimmed === "") return "Name the combo before saving.";
  if (candidates.filter(Boolean).length === 0) return "Add at least one model to the chain.";
  if (candidates.includes(trimmed)) return "A combo cannot list itself as a candidate.";
  if (strategy === "smart" && routerModel === "") return "Smart routing needs a router model.";
  if (strategy === "smart" && routerModel === trimmed) {
    return "A combo cannot use itself as its router.";
  }
  return null;
}

/**
 * Add → Combo, without leaving the preset. The group select writes the same draft entry
 * the row select writes, so the combo lands in that section and saves with the same
 * debounced revisioned PUT — no second assignment step and no second layout writer.
 *
 * The group control is disabled while no editable preset is loaded (no preset, or a
 * conflict); creating the combo itself still works and stays assignable from its row.
 */
export function ComboCreateDialog({
  models,
  groups,
  editable,
  groupDisabledReason,
  busy,
  error,
  onClose,
  onSubmit,
}: {
  models: Model[];
  groups: Array<{ id: string; name: string }>;
  editable: boolean;
  /** Why the group control is off, when it is. */
  groupDisabledReason: string | null;
  busy: boolean;
  error: unknown;
  onClose: () => void;
  onSubmit: (payload: ComboCreatePayload) => void;
}) {
  const form = useRef<HTMLFormElement>(null);
  const [name, setName] = useState("");
  const [chain, setChain] = useState<string[]>([""]);
  const [strategy, setStrategy] = useState<ComboStrategy>("fallback");
  const [routerModel, setRouterModel] = useState("");
  const [group, setGroup] = useState("");
  const problem = comboProblem(name, chain, strategy, routerModel);
  const routers = useMemo(
    () => [...models].sort((a, b) => a.ref.localeCompare(b.ref)),
    [models],
  );

  const submit = () => {
    if (problem !== null) return;
    onSubmit({
      name: name.trim(),
      models: chain.filter(Boolean),
      strategy,
      ...(strategy === "smart" ? { router_model: routerModel } : {}),
      group: group === "" ? null : group,
    });
  };

  return (
    <Modal
      open
      title="New combo"
      description="A named candidate chain. It is added to this preset in the group you choose."
      width="lg"
      onClose={onClose}
      footer={
        <>
          <Button variant="quiet" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button
            variant="primary"
            onClick={() => form.current?.requestSubmit()}
            disabled={problem !== null || busy}
          >
            {busy ? "creating…" : "Create combo"}
          </Button>
        </>
      }
    >
      <form
        ref={form}
        className="flex flex-col gap-4"
        onSubmit={(event) => {
          event.preventDefault();
          submit();
        }}
      >
        <Field label="combo name" hint="the name clients send">
          <Input value={name} onChange={setName} placeholder="research" ariaLabel="combo name" />
          <NamePreviewHint name={name} />
        </Field>

        <Field
          label="preset group"
          hint={
            editable
              ? "The combo is added to this group in the active preset."
              : (groupDisabledReason ?? "Open a preset to assign a group.")
          }
        >
          <Select ariaLabel="preset group" value={group} onChange={setGroup} disabled={!editable}>
            <option value="">Ungrouped (RRouter)</option>
            {groups.map((entry) => (
              <option key={entry.id} value={entry.id}>
                {entry.name}
              </option>
            ))}
          </Select>
        </Field>

        <Field label="strategy">
          <Select
            ariaLabel="strategy"
            value={strategy}
            onChange={(next) => setStrategy(next as ComboStrategy)}
          >
            {STRATEGY_CHOICES.map((entry) => (
              <option key={entry.id} value={entry.id}>
                {entry.label}
              </option>
            ))}
          </Select>
        </Field>

        {strategy === "smart" ? (
          <Field label="router model" hint="the model that picks the first candidate">
            <Select ariaLabel="router model" value={routerModel} onChange={setRouterModel}>
              <option value="" disabled>
                choose a model…
              </option>
              {routers.map((model) => (
                <option key={model.ref} value={model.ref}>
                  {model.display ?? model.id} · {model.provider}
                </option>
              ))}
            </Select>
          </Field>
        ) : null}

        <Field label="candidates" hint="stored top to bottom; the strategy decides who starts">
          <ModelChain models={chain} onChange={setChain} />
        </Field>

        {problem !== null && (name.trim() !== "" || chain.some(Boolean)) ? (
          <p role="alert" className="text-small text-neg">
            {problem}
          </p>
        ) : null}
        {error ? <ErrorNote error={error} /> : null}
      </form>
    </Modal>
  );
}

/** Mounted only while open, so its name field starts empty every time. */
export function SaveAsNewDialog({
  onClose,
  onSubmit,
  busy,
  error,
}: {
  onClose: () => void;
  onSubmit: (name: string) => void;
  busy: boolean;
  error: unknown;
}) {
  const [name, setName] = useState("");
  const ready = name.trim() !== "" && !busy;
  return (
    <Modal
      open
      variant="dialog"
      width="md"
      title="Save as new preset"
      description="Creates a preset from the draft on this screen, including its order, groups and visibility."
      onClose={onClose}
      footer={
        <>
          <Button variant="quiet" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button
            variant="primary"
            onClick={() => ready && onSubmit(name.trim())}
            disabled={!ready}
          >
            {busy ? "creating…" : "Create preset"}
          </Button>
        </>
      }
    >
      <Field label="Preset name">
        <Input value={name} onChange={setName} placeholder="Production" autoFocus />
      </Field>
      {error ? (
        <div className="mt-3">
          <ErrorNote error={error} />
        </div>
      ) : null}
    </Modal>
  );
}

/** The name the gateway will store, when normalising changes what was typed. */
function NamePreviewHint({ name }: { name: string }) {
  const preview = normalisedPreview(name);
  if (!preview) return null;
  return (
    <span className="block font-mono text-caption text-faint">
      saved as <span className="text-muted">{preview}</span>
    </span>
  );
}
