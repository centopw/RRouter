import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Button, Card, CardFooter, CardHeader, ConfirmDialog, ErrorNote, Field, Input, PageHeader } from "../components/ui";
import { DataTable, type Column } from "../components/table";
import { ModelSelect } from "../components/model-select";
import { qk, useAliases } from "../lib/queries";
import { api, type Alias } from "../lib/api";

export default function Aliases() {
  const aliases = useAliases();
  const client = useQueryClient();
  const [name, setName] = useState("");
  const [target, setTarget] = useState("");
  const [pending, setPending] = useState<{ alias: string } | null>(null);

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
        <Button variant="danger" onClick={() => setPending({ alias: r.alias })}>
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

      <Card className="mb-3">
        <form
          onSubmit={(e) => {
            e.preventDefault();
            save.mutate();
          }}
        >
          <div className="grid gap-4 px-5 py-4 sm:grid-cols-[220px_minmax(0,1fr)]">
            <Field label="alias" hint="the name clients send">
              <Input value={name} onChange={setName} placeholder="fast" ariaLabel="alias name" />
            </Field>
            <Field label="target" hint="any model listed in this gateway">
              <ModelSelect value={target} onChange={setTarget} ariaLabel="alias target model" allowEmpty="choose a model" />
            </Field>
          </div>
          {save.isError ? (
            <div className="px-5 pb-3">
              <ErrorNote error={save.error} />
            </div>
          ) : null}
          <CardFooter>
            <Button type="submit" variant="primary" disabled={!name.trim() || !target || save.isPending}>
              save alias
            </Button>
          </CardFooter>
        </form>
      </Card>

      <Card>
        <CardHeader title="Saved aliases" />
        <ConfirmDialog
        open={pending !== null}
        title="Delete alias"
        message={<>Clients calling <span className="font-mono">{pending?.alias}</span> will get a model-not-found error until they use another name.</>}
        confirmLabel="Delete alias"
        busy={remove.isPending}
        onConfirm={() => {
          if (pending) remove.mutate(pending.alias);
          setPending(null);
        }}
        onClose={() => setPending(null)}
      />

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
