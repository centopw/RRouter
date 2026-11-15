import { useMemo, useState } from "react";
import { Link } from "react-router-dom";
import { Copy, PencilSimple, Plus, Star, Trash } from "@phosphor-icons/react";
import {
  Badge,
  Button,
  ConfirmDialog,
  EmptyState,
  ErrorNote,
  Field,
  Input,
  Modal,
  PageHeader,
} from "../components/ui";
import { IconButton } from "../components/atoms";
import { DataTable, type Column } from "../components/table";
import { when } from "../lib/format";
import {
  useCreatePreset,
  useDeletePreset,
  useDuplicatePreset,
  useMakeDefaultPreset,
  useModelProviders,
  usePatchPreset,
  usePresetIndex,
  usePresetLayout,
} from "../lib/queries";
import { groupsOf } from "../lib/layout-draft";
import type { LayoutItem, ModelProvider, PresetSummary } from "../lib/api";

const quietLink =
  "inline-flex h-8 shrink-0 items-center justify-center whitespace-nowrap rounded-full border border-line bg-surface px-3.5 text-body text-muted transition-colors hover:border-line-strong hover:bg-surface-3 hover:text-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text";

const primaryLink =
  "inline-flex h-8 shrink-0 items-center justify-center gap-1.5 whitespace-nowrap rounded-full border border-transparent bg-accent px-3.5 text-body font-semibold text-[var(--color-accent-ink)] transition-colors hover:bg-fg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text";

const textareaClass =
  "w-full resize-y rounded-[var(--radius-input)] border border-line-strong bg-surface px-2.5 py-2 text-body text-fg placeholder:text-faint hover:border-[var(--color-fg)]/40 focus:border-[var(--color-accent-text)] focus:outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-accent-text)]/25";

/** What the name dialog is doing; the endpoint differs, the fields mostly do not. */
type FormState =
  | { kind: "create" }
  | { kind: "edit"; preset: PresetSummary }
  | { kind: "duplicate"; preset: PresetSummary };

type GroupRow = {
  id: string;
  name: string;
  kind: "provider" | "custom";
  provider?: string;
  visible: boolean;
  /** Visible members of the group, and every member of it. */
  shown: number;
  total: number;
  access?: string;
  /** Provider buckets the document never names; derived from its items. */
  implicit: boolean;
};

/**
 * How many models of one group are visible, and what belongs to the group at all. An item
 * with no explicit group falls to its provider bucket, exactly like `groupForEntry`.
 */
function groupCounts(
  items: LayoutItem[],
  explicitIds: Set<string>,
  providerGroup: Map<string, string>,
): Map<string, { shown: number; total: number }> {
  const counts = new Map<string, { shown: number; total: number }>();
  for (const item of items) {
    const key =
      item.group && explicitIds.has(item.group)
        ? item.group
        : (providerGroup.get(item.provider) ?? `provider:${item.provider}`);
    const entry = counts.get(key) ?? { shown: 0, total: 0 };
    entry.total += 1;
    if (item.visible) entry.shown += 1;
    counts.set(key, entry);
  }
  return counts;
}

function accessText(access: { teams?: string[]; keys?: string[]; users?: string[] }): string {
  const parts: string[] = [];
  if (access.teams?.length) parts.push(`${access.teams.length} team${access.teams.length === 1 ? "" : "s"}`);
  if (access.keys?.length) parts.push(`${access.keys.length} key${access.keys.length === 1 ? "" : "s"}`);
  if (access.users?.length) parts.push(`${access.users.length} user${access.users.length === 1 ? "" : "s"}`);
  // An access object with no named grants still restricts: absence means everyone.
  return parts.length > 0 ? parts.join(" · ") : "restricted";
}

/**
 * Presets — the lifecycle surface. A preset is a named routing plan (which models are
 * listed, in what order, in which groups); the plan itself is edited on Models. This page
 * creates, renames, duplicates, defaults, deletes, and shows one plan read-only.
 */
export default function Presets() {
  const presets = usePresetIndex();
  const providers = useModelProviders();

  const createPreset = useCreatePreset();
  const patchPreset = usePatchPreset();
  const duplicatePreset = useDuplicatePreset();
  const makeDefaultPreset = useMakeDefaultPreset();
  const deletePreset = useDeletePreset();

  const [form, setForm] = useState<FormState | null>(null);
  const [formName, setFormName] = useState("");
  const [formDescription, setFormDescription] = useState("");
  const [deleting, setDeleting] = useState<PresetSummary | null>(null);
  const [defaultTarget, setDefaultTarget] = useState<PresetSummary | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);

  const layout = usePresetLayout(selectedId);

  const rows = useMemo(
    () =>
      [...(presets.data?.data ?? [])].sort(
        (a, b) => Number(b.is_default) - Number(a.is_default) || a.name.localeCompare(b.name),
      ),
    [presets.data],
  );
  const selected = rows.find((preset) => preset.id === selectedId) ?? null;
  const providerName = useMemo(
    () => new Map((providers.data ?? []).map((p: ModelProvider) => [p.id, p.display] as const)),
    [providers.data],
  );

  const openForm = (next: FormState) => {
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
    if (!form || formNameValue === "") return;
    const description = formDescription.trim();
    if (form.kind === "create") {
      createPreset.mutate(
        { name: formNameValue, ...(description ? { description } : {}) },
        { onSuccess: closeForm },
      );
      return;
    }
    if (form.kind === "edit") {
      patchPreset.mutate(
        {
          id: form.preset.id,
          patch: { name: formNameValue, description: description === "" ? null : description },
        },
        { onSuccess: closeForm },
      );
      return;
    }
    duplicatePreset.mutate(
      { id: form.preset.id, name: formNameValue },
      { onSuccess: closeForm },
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

  const openDefault = (preset: PresetSummary) => {
    makeDefaultPreset.reset();
    setDefaultTarget(preset);
  };

  const groups: GroupRow[] = useMemo(() => {
    const document = layout.data?.layout;
    if (!document) return [];
    const explicit = groupsOf(document);
    const explicitIds = new Set(explicit.map((group) => group.id));
    const providerGroup = new Map(
      explicit.flatMap((group) => (group.provider ? [[group.provider, group.id] as const] : [])),
    );
    const counts = groupCounts(document.items, explicitIds, providerGroup);
    const out: GroupRow[] = explicit.map((group) => {
      const count = counts.get(group.id) ?? { shown: 0, total: 0 };
      return {
        id: group.id,
        name: group.name,
        kind: group.provider ? "provider" : "custom",
        ...(group.provider ? { provider: group.provider } : {}),
        visible: group.visible,
        shown: count.shown,
        total: count.total,
        access: group.access ? accessText(group.access) : "",
        implicit: false,
      };
    });
    const seen = new Set(out.map((group) => group.id));
    for (const item of document.items) {
      if (item.group && explicitIds.has(item.group)) continue;
      const id = providerGroup.get(item.provider) ?? `provider:${item.provider}`;
      if (seen.has(id)) continue;
      seen.add(id);
      const count = counts.get(id) ?? { shown: 0, total: 0 };
      out.push({
        id,
        name: providerName.get(item.provider) ?? item.provider,
        kind: "provider",
        provider: item.provider,
        visible: true,
        shown: count.shown,
        total: count.total,
        implicit: true,
      });
    }
    return out;
  }, [layout.data, providerName]);

  const columns: Array<Column<PresetSummary>> = [
    {
      key: "name",
      title: "Name",
      render: (preset) => (
        <span className="flex min-w-0 flex-col gap-0.5">
          <span className="flex min-w-0 flex-wrap items-center gap-1.5">
            <button
              type="button"
              onClick={() => setSelectedId(preset.id)}
              className="min-w-0 truncate text-left font-medium text-fg hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text"
              title="Show this preset's read-only layout"
            >
              {preset.name}
            </button>
            {preset.is_default ? <Badge tone="pos">default</Badge> : null}
            {presets.data?.active_id === preset.id ? <Badge tone="accent">active</Badge> : null}
          </span>
          {preset.description ? (
            <span className="truncate text-caption text-faint" title={preset.description}>
              {preset.description}
            </span>
          ) : null}
          {/* Below sm the counts and update time collapse into the name cell. */}
          <span className="flex flex-wrap items-center gap-2 text-caption text-faint sm:hidden">
            <span className="tnum">
              {preset.visible_count} / {preset.entry_count} models
            </span>
            <span>{when(preset.updated_at)}</span>
          </span>
        </span>
      ),
    },
    {
      key: "models",
      title: "Models",
      align: "right",
      width: "6.5rem",
      className: "hidden sm:table-cell",
      render: (preset) => (
        <span
          className="tnum"
          title={`${preset.visible_count} visible of ${preset.entry_count} stored entries; a plan with no stored rows merges the live catalog`}
        >
          <span className="text-fg">{preset.visible_count}</span>
          <span className="text-faint"> / {preset.entry_count}</span>
        </span>
      ),
    },
    {
      key: "default",
      title: "Default",
      width: "6rem",
      className: "hidden sm:table-cell",
      render: (preset) =>
        preset.is_default ? (
          <Badge tone="pos" title="New keys and callers without a preference fall back to this plan">
            default
          </Badge>
        ) : (
          <span className="text-faint">—</span>
        ),
    },
    {
      key: "updated",
      title: "Updated",
      width: "9rem",
      className: "hidden md:table-cell",
      render: (preset) => <span className="tnum text-muted">{when(preset.updated_at)}</span>,
    },
    {
      key: "actions",
      title: "Actions",
      srOnlyTitle: true,
      align: "right",
      width: "12.5rem",
      render: (preset) => (
        <span className="flex items-center justify-end gap-0.5">
          <Link to={`/models?preset=${encodeURIComponent(preset.id)}`} className={quietLink}>
            Open
          </Link>
          <IconButton label={`Rename ${preset.name}`} onClick={() => openForm({ kind: "edit", preset })}>
            <PencilSimple size={15} aria-hidden />
          </IconButton>
          <IconButton
            label={`Duplicate ${preset.name}`}
            onClick={() => openForm({ kind: "duplicate", preset })}
          >
            <Copy size={15} aria-hidden />
          </IconButton>
          <IconButton
            label={`Make ${preset.name} the default`}
            disabled={preset.is_default}
            title={
              preset.is_default
                ? "This preset is already the default"
                : "New keys and callers without a preference fall back to the default"
            }
            onClick={() => openDefault(preset)}
          >
            <Star size={15} aria-hidden />
          </IconButton>
          <IconButton
            label={`Delete ${preset.name}`}
            onClick={() => openDelete(preset)}
            className="hover:bg-[var(--color-neg-tint)] hover:text-[var(--color-neg-ink)]"
          >
            <Trash size={15} aria-hidden />
          </IconButton>
        </span>
      ),
    },
  ];

  const document = layout.data?.layout;
  const visibleItems = document?.items.filter((item) => item.visible).length ?? 0;
  const totalItems = document?.items.length ?? 0;

  return (
    <>
      <PageHeader
        title="Presets"
        subtitle="A preset is a routing plan: which models appear, in what order, and in which groups. Models is the editor; this page owns the lifecycle."
        actions={
          <Button variant="primary" arrow={false} onClick={() => openForm({ kind: "create" })}>
            <Plus size={15} aria-hidden />
            New preset
          </Button>
        }
      />

      {presets.isError ? (
        <div className="mb-4">
          <ErrorNote error={presets.error} onRetry={() => void presets.refetch()} />
        </div>
      ) : null}
      {makeDefaultPreset.error ? (
        <div className="mb-4">
          <ErrorNote error={makeDefaultPreset.error} />
        </div>
      ) : null}

      <div className="overflow-hidden rounded-[var(--radius-card)] border border-line bg-surface">
        <DataTable
          columns={columns}
          rows={presets.data ? rows : undefined}
          loading={presets.isLoading}
          rowKey={(preset) => preset.id}
          rowHeaderKey="name"
          scrollLabel="presets"
          caption="Presets: name, model count, default state and last update"
          empty={
            <div className="flex flex-col items-center gap-2">
              <span>No presets yet. Create one to start a routing plan.</span>
              <Button variant="primary" arrow={false} onClick={() => openForm({ kind: "create" })}>
                <Plus size={15} aria-hidden />
                New preset
              </Button>
            </div>
          }
        />
      </div>

      <Modal
        open={form !== null}
        onClose={closeForm}
        title={formTitle}
        description={formDescriptionText}
        variant="dialog"
        footer={
          <>
            <Button onClick={closeForm} disabled={formPending}>
              cancel
            </Button>
            <Button
              variant="primary"
              arrow={false}
              disabled={formPending || formNameValue === ""}
              onClick={submitForm}
            >
              {formPending
                ? "working…"
                : form?.kind === "create"
                  ? "Create preset"
                  : form?.kind === "edit"
                    ? "Save changes"
                    : "Duplicate preset"}
            </Button>
          </>
        }
      >
        <div className="flex flex-col gap-4">
          {formError ? <ErrorNote error={formError} /> : null}
          <Field
            label="Name"
            hint={
              form?.kind === "duplicate"
                ? `A new name for the copy of ${form.preset.name}.`
                : "Shown in the preset switcher on Models."
            }
          >
            <Input
              value={formName}
              onChange={setFormName}
              autoFocus
              placeholder="e.g. Production"
              ariaLabel="Preset name"
              onKeyDown={(event) => {
                if (event.key === "Enter" && formNameValue !== "" && !formPending) {
                  event.preventDefault();
                  submitForm();
                }
              }}
            />
          </Field>
          {form?.kind === "create" || form?.kind === "edit" ? (
            <Field label="Description" hint="Optional. What this plan is for and who should use it.">
              <textarea
                value={formDescription}
                onChange={(event) => setFormDescription(event.target.value)}
                placeholder="e.g. Fallback chain for production traffic"
                aria-label="Preset description"
                rows={3}
                className={textareaClass}
              />
            </Field>
          ) : null}
        </div>
      </Modal>

      <Modal
        open={selected !== null}
        onClose={() => setSelectedId(null)}
        title={selected?.name ?? "Preset"}
        description="Read-only plan. Ordering, groups and visibility are edited on Models."
        width="lg"
        footer={
          selected ? (
            <>
              <Button onClick={() => setSelectedId(null)}>close</Button>
              <Link to={`/models?preset=${encodeURIComponent(selected.id)}`} className={primaryLink}>
                Open in Models <span aria-hidden>→</span>
              </Link>
            </>
          ) : null
        }
      >
        {selected ? (
          <div className="flex flex-col gap-5">
            {selected.description ? (
              <p className="text-body leading-relaxed text-ink-soft">{selected.description}</p>
            ) : (
              <p className="text-small text-faint">No description.</p>
            )}

            {layout.isError ? (
              <ErrorNote error={layout.error} onRetry={() => void layout.refetch()} />
            ) : layout.isLoading || !document ? (
              <p className="text-small text-faint">loading layout…</p>
            ) : (
              <>
                <dl className="grid grid-cols-2 gap-x-4 gap-y-3 text-small sm:grid-cols-3">
                  <div className="flex min-w-0 flex-col gap-0.5">
                    <dt className="text-caption uppercase tracking-[0.08em] text-faint">revision</dt>
                    <dd className="tnum text-fg">{selected.revision}</dd>
                  </div>
                  <div className="flex min-w-0 flex-col gap-0.5">
                    <dt className="text-caption uppercase tracking-[0.08em] text-faint">updated</dt>
                    <dd className="tnum text-fg">{when(selected.updated_at)}</dd>
                  </div>
                  <div className="flex min-w-0 flex-col gap-0.5">
                    <dt className="text-caption uppercase tracking-[0.08em] text-faint">models</dt>
                    <dd className="tnum text-fg">
                      {visibleItems}
                      <span className="text-faint"> / {totalItems}</span>
                    </dd>
                  </div>
                  <div className="flex min-w-0 flex-col gap-0.5">
                    <dt className="text-caption uppercase tracking-[0.08em] text-faint">new models</dt>
                    <dd>
                      {document.auto_include_new ? (
                        <Badge tone="pos" title="Registry models the plan does not name are merged in">
                          included automatically
                        </Badge>
                      ) : (
                        <Badge tone="neutral" title="Only the models stored in the plan are listed">
                          only listed models
                        </Badge>
                      )}
                    </dd>
                  </div>
                  <div className="flex min-w-0 flex-col gap-0.5">
                    <dt className="text-caption uppercase tracking-[0.08em] text-faint">layout</dt>
                    <dd className="tnum text-fg">v{document.schema_version}</dd>
                  </div>
                  <div className="flex min-w-0 flex-col gap-0.5">
                    <dt className="text-caption uppercase tracking-[0.08em] text-faint">scope</dt>
                    <dd className="text-fg">{selected.scope}</dd>
                  </div>
                </dl>

                <section className="min-w-0">
                  <div className="mb-2 flex items-baseline justify-between gap-3">
                    <h3 className="text-small font-semibold text-ink-soft">Groups</h3>
                    <span className="text-caption text-faint">
                      {groups.length} group{groups.length === 1 ? "" : "s"}
                    </span>
                  </div>
                  {groups.length === 0 ? (
                    <p className="rounded-[var(--radius-input)] border border-line bg-surface-2 px-3 py-2 text-small text-faint">
                      {totalItems === 0
                        ? "This plan stores no items; a document with no stored rows merges the live catalog."
                        : "No groups in this plan."}
                    </p>
                  ) : (
                    <ul className="overflow-hidden rounded-[var(--radius-input)] border border-line">
                      {groups.map((group) => (
                        <li
                          key={group.id}
                          className="flex flex-wrap items-center gap-x-3 gap-y-1 border-t border-line bg-surface px-3 py-2 text-small first:border-t-0"
                        >
                          <span className="min-w-0 flex-1 truncate font-medium text-fg" title={group.id}>
                            {group.name}
                          </span>
                          <span className="text-caption text-faint">
                            {group.kind === "provider" ? `provider · ${group.provider}` : "custom"}
                            {group.implicit ? " (implicit)" : ""}
                          </span>
                          {group.access ? (
                            <Badge tone="warn" title={`restricted to ${group.access}`}>
                              {group.access}
                            </Badge>
                          ) : (
                            <Badge tone="neutral" title="Every caller may use this group">
                              open
                            </Badge>
                          )}
                          {group.visible ? (
                            <Badge tone="neutral" title="Listed by /v1/models">
                              listed
                            </Badge>
                          ) : (
                            <Badge tone="warn" title="Not advertised by /v1/models">
                              hidden
                            </Badge>
                          )}
                          <span className="tnum text-caption text-muted" title="visible / total models">
                            {group.shown} / {group.total}
                          </span>
                        </li>
                      ))}
                    </ul>
                  )}
                </section>

                <p className="text-caption text-faint">
                  Saving order, group membership or visibility happens on Models and bumps the
                  revision; renaming or describing the plan does not.
                </p>
              </>
            )}
          </div>
        ) : (
          <EmptyState>This preset no longer exists.</EmptyState>
        )}
      </Modal>

      <Modal
        open={defaultTarget !== null}
        onClose={() => setDefaultTarget(null)}
        title="Make this preset the default?"
        description="New keys and callers without a preference fall back to the default plan."
        variant="dialog"
        footer={
          <>
            <Button onClick={() => setDefaultTarget(null)} disabled={makeDefaultPreset.isPending}>
              cancel
            </Button>
            <Button
              variant="primary"
              arrow={false}
              disabled={makeDefaultPreset.isPending}
              onClick={() => {
                if (!defaultTarget) return;
                makeDefaultPreset.mutate(defaultTarget.id, {
                  onSuccess: () => setDefaultTarget(null),
                });
              }}
            >
              {makeDefaultPreset.isPending ? "working…" : "Make default"}
            </Button>
          </>
        }
      >
        <div className="flex flex-col gap-3">
          {makeDefaultPreset.error ? <ErrorNote error={makeDefaultPreset.error} /> : null}
          <p className="text-body leading-relaxed text-ink-soft">
            <span className="font-semibold text-fg">{defaultTarget?.name}</span> becomes the
            instance default. The current default keeps its layout and stays available; only
            the fallback changes.
          </p>
        </div>
      </Modal>

      <ConfirmDialog
        open={deleting !== null}
        title="Delete this preset?"
        confirmLabel="Delete preset"
        busy={deletePreset.isPending}
        error={deletePreset.error}
        message={
          <>
            <span className="font-semibold text-fg">{deleting?.name}</span> and its saved order,
            groups and visibility are removed. Models keep routing through the default preset.
            {deleting?.is_default ? (
              <span className="mt-2 block">
                This is the default preset, and the gateway refuses to delete it. Make another
                preset the default first.
              </span>
            ) : null}
          </>
        }
        onClose={() => {
          setDeleting(null);
          deletePreset.reset();
        }}
        onConfirm={() => {
          const target = deleting;
          if (!target) return;
          deletePreset.mutate(target.id, {
            onSuccess: () => {
              if (selectedId === target.id) setSelectedId(null);
              setDeleting(null);
            },
          });
        }}
      />
    </>
  );
}
