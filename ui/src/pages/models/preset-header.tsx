import { useState } from "react";
import * as Popover from "@radix-ui/react-popover";
import { CaretDown, DotsThree } from "@phosphor-icons/react";
import { Link } from "react-router-dom";
import { Button, Dot, ErrorNote, Field, Input, Modal, Select } from "../../components/ui";
import { when } from "../../lib/format";
import type { PresetSummary } from "../../lib/api";
import type { DraftState, LayoutDraft } from "../../lib/layout-draft";

const STATE_TEXT: Record<DraftState, string> = {
  clean: "saved",
  dirty: "unsaved changes",
  saving: "saving…",
  conflict: "not saved · conflict",
};

/**
 * The preset control, not the preset library. Switching and saving live here because they
 * are editing state for this table; rename, duplicate, delete and make-default live on
 * `/routing/presets`, which is the manage link in the menu.
 */
export function PresetControls({
  presets,
  presetId,
  defaultId,
  draft,
  busy,
  onSelect,
  onSaveAsNew,
}: {
  presets: PresetSummary[];
  presetId: string;
  defaultId: string;
  draft: LayoutDraft;
  /** An activation or a pending library mutation. */
  busy: boolean;
  onSelect: (id: string) => void;
  onSaveAsNew: () => void;
}) {
  const [switching, setSwitching] = useState<string | null>(null);
  const dirty = draft.state !== "clean";
  const saving = draft.state === "saving";
  const conflict = draft.conflict !== null;

  const pick = (id: string) => {
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
    <>
      <label className="flex min-w-0 items-center gap-2">
        <span className="text-caption uppercase tracking-[0.08em] text-faint">preset</span>
        <span className="w-[180px] max-w-full">
          <Select ariaLabel="preset being edited" value={presetId} onChange={pick} disabled={busy}>
            {presets.map((preset) => (
              <option key={preset.id} value={preset.id}>
                {preset.name}
                {preset.id === defaultId ? " · default" : ""}
              </option>
            ))}
          </Select>
        </span>
      </label>

      <span
        role="status"
        aria-live="polite"
        className="flex items-center gap-1.5 text-caption text-muted"
      >
        {dirty ? <Dot tone="warn" /> : <Dot tone="pos" />}
        {STATE_TEXT[draft.state]}
        {!dirty && draft.savedAt ? ` · ${when(Math.floor(draft.savedAt / 1000))}` : ""}
      </span>

      <Button
        variant="primary"
        onClick={() => void draft.saveNow()}
        disabled={!dirty || saving || busy || conflict}
        title={
          conflict
            ? "Resolve the conflict below before saving again"
            : dirty
              ? "Write this order and visibility to the preset"
              : "No unsaved changes"
        }
      >
        Save
      </Button>

      <Popover.Root>
        <Popover.Trigger asChild>
          <button
            type="button"
            aria-label="Preset actions"
            aria-haspopup="menu"
            className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full text-muted transition-colors hover:bg-surface-3 hover:text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text"
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
            className="popover-panel z-50 flex w-56 flex-col rounded-[var(--radius-card)] border border-line bg-surface p-1 shadow-lg"
          >
            <Popover.Close asChild>
              <button type="button" role="menuitem" onClick={onSaveAsNew} className={menuItem}>
                Save as new…
              </button>
            </Popover.Close>
            <Popover.Close asChild>
              <Link to="/routing/presets" role="menuitem" className={menuItem}>
                Manage presets…
              </Link>
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
            <Button variant="ghost" onClick={discardAndSwitch}>
              Discard changes
            </Button>
            <Button variant="primary" onClick={() => void saveAndSwitch()}>
              Save & switch
            </Button>
          </>
        }
      >
        <p className="text-body text-muted">
          The selected preset becomes the one this account edits. Saving writes the current
          draft to the preset you are leaving.
        </p>
      </Modal>
    </>
  );
}

/** The one primary action on the page: add a row the registry cannot sync. */
export function AddMenu({ onAddCustom }: { onAddCustom: () => void }) {
  return (
    <Popover.Root>
      <Popover.Trigger asChild>
        <button
          type="button"
          aria-label="Add a model, alias or combo"
          aria-haspopup="menu"
          className="inline-flex h-8 shrink-0 items-center justify-center gap-1.5 whitespace-nowrap rounded-full border border-transparent bg-accent px-3.5 text-body font-semibold text-[var(--color-accent-ink)] transition-colors hover:bg-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text"
        >
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
          className="popover-panel z-50 flex w-56 flex-col rounded-[var(--radius-card)] border border-line bg-surface p-1 shadow-lg"
        >
          <Popover.Close asChild>
            <button type="button" role="menuitem" onClick={onAddCustom} className={menuItem}>
              Custom model
            </button>
          </Popover.Close>
          <Popover.Close asChild>
            <Link to="/routing/aliases" role="menuitem" className={menuItem}>
              Alias
            </Link>
          </Popover.Close>
          <Popover.Close asChild>
            <Link to="/routing/combos" role="menuitem" className={menuItem}>
              Combo
            </Link>
          </Popover.Close>
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
  );
}

const menuItem =
  "flex h-8 items-center rounded-[var(--radius-input)] px-2 text-left text-small text-fg hover:bg-surface-3 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-[-2px] focus-visible:outline-accent-text disabled:cursor-not-allowed disabled:text-faint";

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
