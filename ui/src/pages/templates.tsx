import { useState } from "react";
import { useNavigate } from "react-router-dom";
import {
  Badge,
  Button,
  Card,
  CardHeader,
  EmptyState,
  ErrorNote,
  Field,
  Input,
  Modal,
  ConfirmDialog,
  PageHeader,
} from "../components/ui";
import { DataTable, type Column } from "../components/table";
import { ago, when } from "../lib/format";
import {
  useCreateTemplate,
  useDeleteTemplate,
  useIsAdmin,
  useTemplateVersions,
  useTemplates,
  useUpdateTemplate,
} from "../lib/queries";
import type { Template, TemplateVersion } from "../lib/api";

/**
 * Reusable system prompts: one body an operator writes once and loads into the
 * Playground. The live row carries the newest body; every earlier body stays in the
 * version history, so a restore is an ordinary save that appends a revision.
 */

type Draft = {
  /** null while creating: a non-null id means the save is a PUT. */
  id: string | null;
  name: string;
  body: string;
};

const bodyClass =
  "w-full resize-y rounded-lg border border-line bg-surface-2 px-2.5 py-2 font-mono text-small leading-relaxed text-fg placeholder:text-faint focus:border-[var(--color-accent)]/60 focus:outline-none";

export default function Templates() {
  const templates = useTemplates();
  const isAdmin = useIsAdmin();
  const navigate = useNavigate();
  const [draft, setDraft] = useState<Draft | null>(null);
  const [attempted, setAttempted] = useState(false);
  const [historyOf, setHistoryOf] = useState<Template | null>(null);
  const create = useCreateTemplate();
  const update = useUpdateTemplate();
  const remove = useDeleteTemplate();

  const open = (next: Draft) => {
    setAttempted(false);
    setDraft(next);
  };

  const nameProblem = draft && draft.name.trim() === "" ? "A name is required." : null;
  const bodyProblem = draft && draft.body.trim() === "" ? "A body is required." : null;

  const save = () => {
    if (!draft) return;
    if (nameProblem || bodyProblem) {
      setAttempted(true);
      return;
    }
    const name = draft.name.trim();
    const body = draft.body;
    if (draft.id === null) {
      create.mutate({ name, body }, { onSuccess: () => setDraft(null) });
    } else {
      update.mutate({ id: draft.id, name, body }, { onSuccess: () => setDraft(null) });
    }
  };

  const [deleting, setDeleting] = useState<Template | null>(null);
  const del = (row: Template) => setDeleting(row);

  /** A restored body is a PUT, so the server records it as the next version. */
  const restore = (row: Template, version: TemplateVersion) => {
    setHistoryOf(null);
    open({ id: row.id, name: row.name, body: version.body });
  };

  const rows = templates.data;
  const columns: Array<Column<Template>> = [
    {
      key: "name",
      title: "name",
      render: (row) => {
        const line = row.body.replace(/\s+/g, " ").trim();
        return (
          <div className="min-w-0">
            <div className="font-medium text-fg">{row.name}</div>
            {line ? (
              <div className="mt-0.5 max-w-[420px] truncate font-mono text-caption text-faint">
                {line}
              </div>
            ) : null}
          </div>
        );
      },
    },
    {
      key: "version",
      title: "version",
      width: "90px",
      render: (row) => <Badge tone="neutral">v{row.version}</Badge>,
    },
    {
      key: "updated",
      title: "updated",
      width: "150px",
      render: (row) => (
        <span className="text-small text-muted" title={when(row.updated_at)}>
          {ago(row.updated_at)}
        </span>
      ),
    },
    {
      key: "actions",
      title: "",
      align: "right",
      render: (row) => (
        <span className="flex flex-wrap items-center justify-end gap-1.5">
          {isAdmin ? (
            <>
              <Button
                variant="ghost"
                onClick={() => navigate(`/playground?template=${encodeURIComponent(row.id)}`)}
                title="load this prompt into the Playground and open it"
              >
                open
              </Button>
              <Button
                variant="quiet"
                onClick={() => open({ id: row.id, name: row.name, body: row.body })}
              >
                edit
              </Button>
            </>
          ) : null}
          <Button
            variant="quiet"
            onClick={() => setHistoryOf(row)}
            title="every body revision, newest first"
          >
            history
          </Button>
          {isAdmin ? (
            <Button variant="danger" onClick={() => del(row)}>
              delete
            </Button>
          ) : null}
        </span>
      ),
    },
  ];

  return (
    <>
      <ConfirmDialog
        open={deleting !== null}
        title="Delete template"
        message={<>Delete <span className="font-medium">{deleting?.name}</span> and its whole version history? Playground prompts that used it are unaffected, but the template cannot be restored.</>}
        confirmLabel="Delete template"
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

      <PageHeader
        title="Templates"
        subtitle={
          isAdmin
            ? "Reusable system prompts. Write one here, then load it into the Playground."
            : "Reusable system prompts, newest revision first. Every earlier body is kept in the history."
        }
        actions={
          isAdmin ? (
            <Button variant="primary" onClick={() => open({ id: null, name: "", body: "" })}>
              New template
            </Button>
          ) : undefined
        }
      />

      {templates.isError ? (
        <div className="mb-4">
          <ErrorNote error={templates.error} onRetry={() => void templates.refetch()} />
        </div>
      ) : null}

      {remove.isError ? (
        <div className="mb-4">
          <ErrorNote error={remove.error} />
        </div>
      ) : null}

      <Card>
        <CardHeader
          title="Saved templates"
          action={
            <span className="text-caption text-faint">
              {rows ? `${rows.length} ${rows.length === 1 ? "template" : "templates"}` : null}
            </span>
          }
        />
        {rows && rows.length === 0 ? (
          <EmptyState>
            <p className="mx-auto max-w-[440px] text-body text-muted">
              A template is a reusable system prompt: the instructions a model reads before the
              conversation. Save one here and load it into the Playground instead of retyping
              it for every test.
            </p>
            {isAdmin ? (
              <div className="mt-3">
                <Button variant="ghost" onClick={() => open({ id: null, name: "", body: "" })}>
                  write the first template
                </Button>
              </div>
            ) : null}
          </EmptyState>
        ) : (
          <DataTable
            columns={columns}
            rows={rows}
            rowKey={(row) => row.id}
            loading={templates.isLoading}
            empty="No templates yet."
          />
        )}
      </Card>

      {draft ? (
        <Modal
          title={draft.id === null ? "new template" : "edit template"}
          open
          onClose={() => setDraft(null)}
          footer={
            <>
              <Button onClick={() => setDraft(null)}>cancel</Button>
              <Button
                variant="primary"
                disabled={create.isPending || update.isPending}
                onClick={save}
              >
                {create.isPending || update.isPending ? "saving…" : "save"}
              </Button>
            </>
          }
        >
          <div className="space-y-3">
            <Field label="name" hint="unique; the Playground picker shows this">
              <Input
                value={draft.name}
                onChange={(name) => setDraft({ ...draft, name })}
                placeholder="support-reply"
                ariaLabel="template name"
              />
            </Field>
            {attempted && nameProblem ? (
              <p className="text-small text-neg">{nameProblem}</p>
            ) : null}

            <Field
              label="body"
              hint="the system prompt itself; saving a changed body creates a new version"
            >
              <textarea
                value={draft.body}
                onChange={(event) => setDraft({ ...draft, body: event.target.value })}
                rows={12}
                aria-label="template body"
                placeholder="You are a concise support assistant. Answer in the customer's language…"
                className={bodyClass}
              />
            </Field>
            {attempted && bodyProblem ? (
              <p className="text-small text-neg">{bodyProblem}</p>
            ) : null}

            {draft.id !== null ? (
              <p className="font-mono text-caption text-faint">
                editing v{
                  rows?.find((row) => row.id === draft.id)?.version ?? "?"
                } · a name change alone keeps the version
              </p>
            ) : null}

            {create.isError ? <ErrorNote error={create.error} /> : null}
            {update.isError ? <ErrorNote error={update.error} /> : null}
          </div>
        </Modal>
      ) : null}

      {historyOf ? (
        <TemplateHistory
          template={historyOf}
          onClose={() => setHistoryOf(null)}
          onRestore={(version) => restore(historyOf, version)}
        />
      ) : null}
    </>
  );
}

/** Mounted only while open, so the revision list never loads for a closed drawer. */
function TemplateHistory({
  template,
  onClose,
  onRestore,
}: {
  template: Template;
  onClose: () => void;
  onRestore: (version: TemplateVersion) => void;
}) {
  const versions = useTemplateVersions(template.id);

  return (
    <Modal
      title={`history — ${template.name}`}
      open
      onClose={onClose}
      footer={<Button onClick={onClose}>close</Button>}
    >
      <p className="mb-3 text-small text-faint">
        Newest first. Restore copies that body into the editor; saving it publishes a new
        version, so nothing is lost.
      </p>

      {versions.isError ? <ErrorNote error={versions.error} onRetry={() => void versions.refetch()} /> : null}

      {!versions.data ? (
        <p className="py-6 text-center text-body text-faint">
          {versions.isLoading ? "loading…" : "—"}
        </p>
      ) : versions.data.length === 0 ? (
        <p className="py-6 text-center text-body text-faint">No revisions recorded.</p>
      ) : (
        <ol className="space-y-2">
          {versions.data.map((version) => (
            <li
              key={version.version}
              className="rounded-lg border border-line bg-surface-2 px-3 py-2"
            >
              <div className="flex flex-wrap items-center justify-between gap-2">
                <span className="flex items-center gap-2">
                  <Badge tone={version.version === template.version ? "pos" : "neutral"}>
                    v{version.version}
                  </Badge>
                  <span className="text-caption text-faint" title={when(version.created_at)}>
                    {when(version.created_at)} · {ago(version.created_at)}
                  </span>
                  {version.version === template.version ? (
                    <span className="text-caption text-faint">current</span>
                  ) : null}
                </span>
                <Button variant="ghost" onClick={() => onRestore(version)}>
                  restore
                </Button>
              </div>
              <p className="mt-1.5 max-h-24 overflow-hidden whitespace-pre-wrap break-words font-mono text-caption leading-relaxed text-muted">
                {version.body}
              </p>
            </li>
          ))}
        </ol>
      )}
    </Modal>
  );
}
