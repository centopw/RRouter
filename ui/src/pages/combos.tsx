import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Button, Card, CardHeader, ErrorNote, Field, Input, PageHeader } from "../components/ui";
import { DataTable, type Column } from "../components/table";
import { ModelChain } from "../components/model-select";
import { qk, useCombos } from "../lib/queries";
import { api, type Combo } from "../lib/api";

export default function Combos() {
  const combos = useCombos();
  const client = useQueryClient();
  const [name, setName] = useState("");
  const [models, setModels] = useState<string[]>([""]);

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
      invalidate();
    },
  });

  const remove = useMutation({
    mutationFn: (combo: string) => api.del<{ deleted: boolean }>(`/api/combos?name=${encodeURIComponent(combo)}`),
    onSuccess: invalidate,
  });

  const columns: Array<Column<Combo>> = [
    { key: "name", title: "name", render: (r) => <span className="font-mono text-[12px]">{r.name}</span> },
    {
      key: "models",
      title: "fallback order",
      render: (r) => (
        <span className="flex flex-wrap items-center gap-1.5">
          {r.models.map((m, i) => (
            <span key={`${m}-${i}`} className="flex items-center gap-1.5">
              {i > 0 ? <span className="text-faint">→</span> : null}
              <span className="rounded border border-line-strong bg-surface-2 px-1.5 py-0.5 font-mono text-[11px] text-muted">
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
        <Button variant="danger" onClick={() => remove.mutate(r.name)}>
          delete
        </Button>
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

      <Card className="mb-3 px-5 py-4">
        <form
          className="flex flex-col gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            save.mutate();
          }}
        >
          <div className="flex flex-wrap items-end gap-3">
            <div className="w-[220px]">
              <Field label="combo name" hint="the name clients send">
                <Input value={name} onChange={setName} placeholder="smart" ariaLabel="combo name" />
              </Field>
            </div>
            <Button
              type="submit"
              variant="primary"
              disabled={!name.trim() || !models.some(Boolean) || save.isPending}
            >
              save combo
            </Button>
          </div>
          <Field label="fallback order" hint="tried top to bottom">
            <ModelChain models={models} onChange={setModels} />
          </Field>
        </form>
        {save.isError ? (
          <div className="mt-3">
            <ErrorNote error={save.error} />
          </div>
        ) : null}
      </Card>

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
