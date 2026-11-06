import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Badge, Button, Card, CardHeader, CopyChip, ErrorNote, Input, PageHeader } from "../components/ui";
import { DataTable, type Column } from "../components/table";
import { when } from "../lib/format";
import { qk, useKeys } from "../lib/queries";
import { api, type KeyResult, type NewKey } from "../lib/api";

export default function Keys() {
  const keys = useKeys();
  const client = useQueryClient();
  const [name, setName] = useState("");
  const [issued, setIssued] = useState<NewKey | null>(null);

  const create = useMutation({
    mutationFn: () => api.post<NewKey>("/api/keys", { name: name || null }),
    onSuccess: (key) => {
      setIssued(key);
      setName("");
      void client.invalidateQueries({ queryKey: qk.keys });
      void client.invalidateQueries({ queryKey: qk.system });
    },
  });

  const remove = useMutation({
    mutationFn: (id: string) => api.del<{ deleted: boolean }>(`/api/keys/${id}`),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: qk.keys });
      void client.invalidateQueries({ queryKey: qk.system });
    },
  });

  const columns: Array<Column<KeyResult>> = [
    { key: "name", title: "name", render: (r) => r.name ?? <span className="text-faint">unnamed</span> },
    { key: "id", title: "id", render: (r) => <span className="font-mono text-[12px] text-muted">{r.id}</span> },
    { key: "created", title: "created", render: (r) => <span className="text-muted">{when(r.created_at)}</span> },
    {
      key: "actions",
      title: "",
      align: "right",
      render: (r) => (
        <Button
          variant="danger"
          onClick={() => {
            if (confirm(`Revoke key ${r.name ?? r.id}? Clients using it stop working immediately.`)) {
              remove.mutate(r.id);
            }
          }}
        >
          revoke
        </Button>
      ),
    },
  ];

  return (
    <>
      <PageHeader
        title="API Keys"
        subtitle="Clients authenticate with these against /v1. Keys are stored hashed; the plaintext is shown once."
      />

      {keys.isError ? (
        <div className="mb-4">
          <ErrorNote error={keys.error} onRetry={() => void keys.refetch()} />
        </div>
      ) : null}

      {issued ? (
        <Card className="mb-3 border-[#00d4ff]/35 bg-[#00d4ff]/10 px-5 py-4">
          <div className="flex items-center justify-between gap-3">
            <div className="min-w-0">
              <div className="text-[12px] text-[#67e8f9]">Copy this key now — it is not shown again.</div>
              <code className="mt-1 block truncate font-mono text-[13px] text-fg">{issued.key}</code>
            </div>
            <Button
              variant="primary"
              onClick={() => {
                void navigator.clipboard.writeText(issued.key);
              }}
            >
              copy
            </Button>
          </div>
        </Card>
      ) : null}

      <Card>
        <CardHeader
          title="Keys"
          action={
            <form
              className="flex items-center gap-2"
              onSubmit={(e) => {
                e.preventDefault();
                create.mutate();
              }}
            >
              <Input value={name} onChange={setName} placeholder="key name" ariaLabel="new key name" className="w-[200px]" />
              <Button type="submit" variant="primary" disabled={create.isPending}>
                create key
              </Button>
            </form>
          }
        />
        {create.isError ? (
          <div className="px-5 pb-3">
            <ErrorNote error={create.error} />
          </div>
        ) : null}
        <DataTable
          columns={columns}
          rows={keys.data}
          rowKey={(r) => r.id}
          loading={keys.isLoading}
          empty="No keys yet. Create one to require authentication."
        />
        <div className="flex flex-wrap items-center gap-2 border-t border-line px-5 py-3 text-[11px] text-faint">
          <span>endpoint</span>
          <CopyChip value={`${window.location.origin}/v1`} />
          <span className="ml-auto flex items-center gap-1.5">
            enforcement is controlled by <Badge>require_api_key</Badge> in the server config
          </span>
        </div>
      </Card>
    </>
  );
}
