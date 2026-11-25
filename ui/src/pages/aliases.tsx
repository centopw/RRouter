import { useEffect, useRef, useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { PencilSimple, Plus, Trash } from "@phosphor-icons/react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import {
  Button,
  Card,
  CardHeader,
  ConfirmDialog,
  ErrorNote,
  Field,
  Input,
  Modal,
  PageHeader,
} from "../components/ui";
import { IconButton } from "../components/atoms";
import { DataTable, type Column } from "../components/table";
import { ModelSelect } from "../components/model-select";
import { qk, useAliases } from "../lib/queries";
import { api, type Alias } from "../lib/api";
import { normalisedPreview } from "../lib/model-name";

export default function Aliases() {
  const aliases = useAliases();
  const client = useQueryClient();
  const [params, setParams] = useSearchParams();
  const formRef = useRef<HTMLFormElement>(null);
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [name, setName] = useState("");
  const [target, setTarget] = useState("");
  const [pending, setPending] = useState<{ alias: string } | null>(null);
  const [editing, setEditing] = useState(false);

  const invalidate = () => {
    void client.invalidateQueries({ queryKey: qk.aliases });
    void client.invalidateQueries({ queryKey: qk.models });
    void client.invalidateQueries({ queryKey: qk.system });
  };

  const reset = () => {
    setName("");
    setTarget("");
    setEditing(false);
    setDrawerOpen(false);
  };

  const openCreate = () => {
    setName("");
    setTarget("");
    setEditing(false);
    save.reset();
    setDrawerOpen(true);
  };

  const openEdit = (row: Alias) => {
    setName(row.alias);
    setTarget(row.target);
    setEditing(true);
    save.reset();
    setDrawerOpen(true);
  };

  const editParam = params.get("edit");
  useEffect(() => {
    if (!editParam || !aliases.data) return;
    const match = aliases.data.find((item) => item.alias === editParam);
    if (match) openEdit(match);
    const next = new URLSearchParams(params);
    next.delete("edit");
    setParams(next, { replace: true });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [editParam, aliases.data]);

  const save = useMutation({
    mutationFn: () => api.post<Alias>("/api/aliases", { alias: name.trim(), target }),
    onSuccess: () => {
      reset();
      invalidate();
    },
  });

  const remove = useMutation({
    mutationFn: (alias: string) => api.del<{ deleted: boolean }>(`/api/aliases?alias=${encodeURIComponent(alias)}`),
    onSuccess: invalidate,
  });

  const aliasList = aliases.data ?? [];

  const columns: Array<Column<Alias>> = [
    { key: "alias", title: "Alias", render: (r) => <span className="font-mono text-small">{r.alias}</span> },
    {
      key: "target",
      title: "Target",
      render: (r) => <span className="font-mono text-small text-muted">{r.target}</span>,
    },
    {
      key: "actions",
      title: "Actions",
      srOnlyTitle: true,
      align: "right",
      width: "14rem",
      render: (r) => (
        <span className="flex items-center justify-end gap-1">
          <Link
            to={`/playground/chat?model=${encodeURIComponent(r.alias)}`}
            className="inline-flex h-8 shrink-0 items-center whitespace-nowrap rounded-full border border-line bg-surface px-2.5 text-caption font-medium text-fg transition-colors hover:border-line-strong hover:bg-surface-3 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent-text"
          >
            Test in Playground →
          </Link>
          <IconButton label={`Edit alias ${r.alias}`} onClick={() => openEdit(r)}>
            <PencilSimple size={15} aria-hidden />
          </IconButton>
          <IconButton
            label={`Delete alias ${r.alias}`}
            onClick={() => setPending({ alias: r.alias })}
            className="hover:bg-[var(--color-neg-tint)] hover:text-[var(--color-neg-ink)]"
          >
            <Trash size={15} aria-hidden />
          </IconButton>
        </span>
      ),
    },
  ];

  return (
    <>
      <PageHeader
        title="Aliases"
        subtitle="A stable name clients can call. Point it at any model this gateway serves."
        actions={
          <Button variant="primary" onClick={openCreate}>
            <Plus size={15} aria-hidden />
            Create alias
          </Button>
        }
      />

      {aliases.isError ? (
        <div className="mb-4">
          <ErrorNote error={aliases.error} onRetry={() => void aliases.refetch()} />
        </div>
      ) : null}

      <Modal
        open={drawerOpen}
        onClose={() => {
          if (!save.isPending) reset();
        }}
        title={editing ? `Edit alias · ${name}` : "Create alias"}
        description="Point a stable model name at any model this gateway serves."
        width="md"
        footer={
          <>
            <Button variant="quiet" onClick={reset} disabled={save.isPending}>
              Cancel
            </Button>
            <Button
              variant="primary"
              onClick={() => formRef.current?.requestSubmit()}
              disabled={!name.trim() || !target || save.isPending}
            >
              {save.isPending ? "Saving…" : editing ? "Update alias" : "Save alias"}
            </Button>
          </>
        }
      >
        <form
          ref={formRef}
          className="flex flex-col gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            if (name.trim() && target && !save.isPending) save.mutate();
          }}
        >
          <Field label="Alias" hint="The name clients send">
            <Input
              value={name}
              onChange={setName}
              placeholder="fast"
              ariaLabel="alias name"
              autoFocus
              disabled={save.isPending}
            />
            <NamePreviewHint name={name} />
          </Field>
          <Field label="Target" hint="Any model listed in this gateway">
            <ModelSelect
              value={target}
              onChange={setTarget}
              ariaLabel="alias target model"
              allowEmpty="Choose a model"
              disabled={save.isPending}
            />
          </Field>
          {save.isError ? <ErrorNote error={save.error} /> : null}
        </form>
      </Modal>

      <Card>
        <CardHeader
          title="Saved aliases"
          action={
            <span className="text-caption text-faint tnum">
              {aliasList.length} total
            </span>
          }
        />
        <ConfirmDialog
          open={pending !== null}
          title="Delete alias"
          message={
            <>
              Clients calling <span className="font-mono">{pending?.alias}</span> will get a
              model-not-found error until they use another name.
            </>
          }
          confirmLabel="Delete alias"
          busy={remove.isPending}
          error={remove.error}
          onConfirm={() => {
            if (pending) {
              remove.mutate(pending.alias, { onSuccess: () => setPending(null) });
            }
          }}
          onClose={() => {
            if (!remove.isPending) {
              remove.reset();
              setPending(null);
            }
          }}
        />

        <DataTable
          columns={columns}
          rows={aliases.data}
          rowKey={(r) => r.alias}
          loading={aliases.isLoading}
          tableLayout="fixed"
          stickyRight="actions"
          empty={
            <div className="flex flex-col items-center gap-2">
              <span>No aliases. Clients can still call provider/model directly.</span>
              <Button variant="primary" onClick={openCreate}>
                <Plus size={15} aria-hidden />
                Create alias
              </Button>
            </div>
          }
        />
      </Card>
    </>
  );
}

/**
 * The name this form will actually store, when it differs from what was typed: the
 * gateway normalises every combo/alias name, so say so before saving rather than after.
 */
function NamePreviewHint({ name }: { name: string }) {
  const preview = normalisedPreview(name);
  if (!preview) return null;
  return (
    <span className="block font-mono text-caption text-faint">
      saved as <span className="text-muted">{preview}</span>
    </span>
  );
}
