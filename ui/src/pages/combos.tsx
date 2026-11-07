import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Button, Card, CardFooter, CardHeader, ConfirmDialog, ErrorNote, Field, Input, PageHeader } from "../components/ui";
import { DataTable, type Column } from "../components/table";
import { ModelChain } from "../components/model-select";
import { qk, useCombos } from "../lib/queries";
import { api, type Combo } from "../lib/api";

export default function Combos() {
  const combos = useCombos();
  const client = useQueryClient();
  const [name, setName] = useState("");
  const [models, setModels] = useState<string[]>([""]);
  const [pending, setPending] = useState<string | null>(null);
  const [editing, setEditing] = useState(false);

  const invalidate = () => {
    void client.invalidateQueries({ queryKey: qk.combos });
    void client.invalidateQueries({ queryKey: qk.models });
    void client.invalidateQueries({ queryKey: qk.system });
  };

  const save = useMutation({
    mutationFn: () =>
      api.post<Combo>("/api/combos", { name: name.trim(), models: models.filter(Boolean) }),
    onSuccess: () => {
      setName("");
      setModels([""]);
      setEditing(false);
      invalidate();
    },
  });

  const remove = useMutation({
    mutationFn: (combo: string) => api.del<{ deleted: boolean }>(`/api/combos?name=${encodeURIComponent(combo)}`),
    onSuccess: invalidate,
  });

  const columns: Array<Column<Combo>> = [
    { key: "name", title: "name", render: (r) => <span className="font-mono text-small">{r.name}</span> },
    {
      key: "models",
      title: "fallback order",
      render: (r) => (
        <span className="flex flex-wrap items-center gap-1.5">
          {r.models.map((m, i) => (
            <span key={`${m}-${i}`} className="flex items-center gap-1.5">
              {i > 0 ? <span className="text-faint">→</span> : null}
              <span className="rounded border border-line-strong bg-surface-2 px-1.5 py-0.5 font-mono text-caption text-muted">
                {m}
              </span>
            </span>
          ))}
        </span>
      ),
    },
    {
      key: "actions",
      title: "",
      align: "right",
      render: (r) => (
        <span className="flex justify-end gap-1.5">
          <Button
            onClick={() => {
              setName(r.name);
              setModels(r.models.length > 0 ? r.models : [""]);
              setEditing(true);
            }}
          >
            edit
          </Button>
          <Button variant="danger" onClick={() => setPending(r.name)}>
            delete
          </Button>
        </span>
      ),
    },
  ];

  return (
    <>
      <PageHeader
        title="Combos"
        subtitle="An ordered fallback chain. The gateway tries each model in turn when one fails."
      />

      {combos.isError ? (
        <div className="mb-4">
          <ErrorNote error={combos.error} onRetry={() => void combos.refetch()} />
        </div>
      ) : null}

      <Card className="mb-3">
        <form
          onSubmit={(e) => {
            e.preventDefault();
            save.mutate();
          }}
        >
          <div className="flex flex-col gap-4 px-5 py-4">
            <div className="sm:w-[280px]">
              <Field label="combo name" hint="the name clients send">
                <Input value={name} onChange={setName} placeholder="smart" ariaLabel="combo name" />
              </Field>
            </div>
            <Field label="fallback order" hint="tried top to bottom">
              <ModelChain models={models} onChange={setModels} />
            </Field>
          </div>
          {save.isError ? (
            <div className="px-5 pb-3">
              <ErrorNote error={save.error} />
            </div>
          ) : null}
          <CardFooter>
            {editing ? (
              <Button
                onClick={() => {
                  setName("");
                  setModels([""]);
                  setEditing(false);
                }}
              >
                cancel
              </Button>
            ) : null}
            <Button type="submit" variant="primary" disabled={!name.trim() || !models.some(Boolean) || save.isPending}>
              {editing ? "update combo" : "save combo"}
            </Button>
          </CardFooter>
        </form>
      </Card>

      <ConfirmDialog
        open={pending !== null}
        title="Delete combo"
        message={<>The fallback chain <span className="font-mono">{pending}</span> stops existing. Requests naming it fail until they use another model.</>}
        confirmLabel="Delete combo"
        busy={remove.isPending}
        onConfirm={() => {
          if (pending) remove.mutate(pending);
          setPending(null);
        }}
        onClose={() => setPending(null)}
      />

      <Card>
        <CardHeader title="Saved combos" />
        <DataTable
          columns={columns}
          rows={combos.data}
          rowKey={(r) => r.name}
          loading={combos.isLoading}
          empty="No combos. One model per request is the default."
        />
      </Card>
    </>
  );
}
