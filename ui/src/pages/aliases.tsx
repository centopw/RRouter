import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Button, Card, CardHeader, ErrorNote, Field, Input, PageHeader } from "../components/ui";
import { DataTable, type Column } from "../components/table";
import { ModelSelect } from "../components/model-select";
import { qk, useAliases } from "../lib/queries";
import { api, type Alias } from "../lib/api";

export default function Aliases() {
  const aliases = useAliases();
  const client = useQueryClient();
  const [name, setName] = useState("");
  const [target, setTarget] = useState("");

  const invalidate = () => {
    void client.invalidateQueries({ queryKey: qk.aliases });
    void client.invalidateQueries({ queryKey: qk.models });
    void client.invalidateQueries({ queryKey: qk.system });
  };

  const save = useMutation({
    mutationFn: () => api.post<Alias>("/api/aliases", { alias: name.trim(), target }),
    onSuccess: () => {
      setName("");
      setTarget("");
      invalidate();
    },
  });

  const remove = useMutation({
    mutationFn: (alias: string) => api.del<{ deleted: boolean }>(`/api/aliases?alias=${encodeURIComponent(alias)}`),
    onSuccess: invalidate,
  });

  const columns: Array<Column<Alias>> = [
    { key: "alias", title: "alias", render: (r) => <span className="font-mono text-[12px]">{r.alias}</span> },
    {
      key: "target",
      title: "target",
      render: (r) => <span className="font-mono text-[12px] text-muted">{r.target}</span>,
    },
    {
      key: "actions",
      title: "",
      align: "right",
      render: (r) => (
        <Button variant="danger" onClick={() => remove.mutate(r.alias)}>
          delete
        </Button>
      ),
    },
  ];

  return (
    <>
      <PageHeader
        title="Aliases"
        subtitle="A stable name clients can call. Point it at any model this gateway serves."
      />

      {aliases.isError ? (
        <div className="mb-4">
          <ErrorNote error={aliases.error} onRetry={() => void aliases.refetch()} />
        </div>
      ) : null}

      <Card className="mb-3 px-5 py-4">
        <form
          className="flex flex-wrap items-end gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            save.mutate();
          }}
        >
          <div className="w-[220px]">
            <Field label="alias" hint="the name clients send">
              <Input value={name} onChange={setName} placeholder="fast" ariaLabel="alias name" />
            </Field>
          </div>
          <div className="w-[320px]">
            <Field label="target" hint="any model listed in this gateway">
              <ModelSelect value={target} onChange={setTarget} ariaLabel="alias target model" allowEmpty="choose a model" />
            </Field>
          </div>
          <Button type="submit" variant="primary" disabled={!name.trim() || !target || save.isPending}>
            save alias
          </Button>
        </form>
        {save.isError ? (
          <div className="mt-3">
            <ErrorNote error={save.error} />
          </div>
        ) : null}
      </Card>

      <Card>
        <CardHeader title="Saved aliases" />
        <DataTable
          columns={columns}
          rows={aliases.data}
          rowKey={(r) => r.alias}
          loading={aliases.isLoading}
          empty="No aliases. Clients can still call provider/model directly."
        />
      </Card>
    </>
  );
}
