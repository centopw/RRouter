import { useMemo, useRef, useState } from "react";
import * as Popover from "@radix-ui/react-popover";
import { CaretDown, DotsThree } from "@phosphor-icons/react";
import { Link } from "react-router-dom";
import { Button, Dot, ErrorNote, Field, Input, Modal, Select } from "../../components/ui";
import { ModelChain } from "../../components/model-select";
import { when } from "../../lib/format";
import type { ComboStrategy, Model, PresetSummary } from "../../lib/api";
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
export function AddMenu({
  onAddCustom,
  onAddCombo,
}: {
  onAddCustom: () => void;
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
            <button type="button" role="menuitem" onClick={onAddCombo} className={menuItem}>
              Combo
            </button>
          </Popover.Close>
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
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
